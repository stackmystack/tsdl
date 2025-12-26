use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, create_dir_all},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use tokio::time;
use url::Url;

use crate::{
    app::App,
    args::ParserConfig,
    cache::{self, Cache},
    consts::TSDL_FROM,
    display::{Handle, Progress, ProgressState, TICK_CHARS},
    error,
    error::TsdlError,
    git::Ref,
    parser::{build_languages, Language, NUM_STEPS},
    tree_sitter, SafeCanonicalize, TsdlResult,
};

pub fn run(app: &App) -> TsdlResult<()> {
    if app.command.show_config {
        crate::config::show(&app.command)?;
    }
    clear(app)?;
    build_impl(app)?;
    Ok(())
}

fn clear(app: &App) -> TsdlResult<()> {
    if app.command.fresh && app.command.build_dir.exists() {
        let mut progress = app
            .progress
            .lock()
            .map_err(|e| TsdlError::message(format!("Failed to acquire progress lock: {e}")))?;
        let handle = progress.register("Fresh Build", 1);
        let disp = &app.command.build_dir.display();
        fs::remove_dir_all(&app.command.build_dir)?;
        handle.fin(format!("Cleaned {disp}"));
    }
    fs::create_dir_all(&app.command.build_dir)?;
    Ok(())
}

fn build_impl(app: &App) -> TsdlResult<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(app.command.ncpus)
        .build()?;
    let _guard = rt.enter();
    rt.spawn(update_screen(app.progress.clone()));
    let ts_cli = rt.block_on(tree_sitter::prepare(&app.command, app.progress.clone()))?;

    // Load cache from disk
    let cache = Cache::load(&app.command.build_dir)?;
    let cache = Arc::new(Mutex::new(cache));

    let languages = collect_languages(
        app,
        ts_cli,
        app.command.languages.as_ref(),
        app.command.parsers.as_ref(),
        &cache,
    )?;
    create_dir_all(&app.command.out_dir)?;

    // Build and then save cache
    let result = rt.block_on(async {
        let result = build_languages(languages).await;

        // Save cache to disk after build completes (success or failure)
        if let Ok(cache_guard) = cache.try_lock() {
            cache_guard.save(&app.command.build_dir).ok();
        }

        result
    });

    result
}

async fn update_screen(progress: Arc<Mutex<Progress>>) {
    let mut interval = time::interval(time::Duration::from_millis(
        1000 / TICK_CHARS.chars().count() as u64,
    ));
    loop {
        interval.tick().await;
        if let Ok(s) = progress.try_lock() {
            s.tick();
        }
    }
}

fn collect_languages(
    app: &App,
    ts_cli: PathBuf,
    requested_languages: Option<&Vec<String>>,
    defined_parsers: Option<&BTreeMap<String, ParserConfig>>,
    cache: &Arc<Mutex<cache::Cache>>,
) -> Result<Vec<Language>, error::LanguageCollection> {
    let results = unique_languages(app, ts_cli, requested_languages, defined_parsers, cache);
    let (ok, err): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);

    if err.is_empty() {
        Ok(ok.into_iter().map(Result::unwrap).collect())
    } else {
        Err(error::LanguageCollection {
            related: err.into_iter().map(Result::unwrap_err).collect(),
        })
    }
}

fn unique_languages(
    app: &App,
    ts_cli: PathBuf,
    requested_languages: Option<&Vec<String>>,
    defined_parsers: Option<&BTreeMap<String, ParserConfig>>,
    cache: &Arc<Mutex<cache::Cache>>,
) -> Vec<Result<Language, error::Language>> {
    let ts_cli = Arc::new(ts_cli);
    let final_languages = match requested_languages {
        Some(langs) if !langs.is_empty() => langs.clone(),
        _ => defined_parsers
            .map(|parsers| parsers.keys().cloned().collect())
            .unwrap_or_default(),
    };

    let unique = final_languages.into_iter().collect::<HashSet<_>>();
    let mut results = Vec::new();

    for language in unique {
        let (build_script, git_ref, url) = get_language_coords(&language, defined_parsers);
        let result = match url {
            Ok(repo) => Ok(Language::new(
                app.command
                    .build_dir
                    .join(format!("tree-sitter-{}", &language)) // make sure it follows this format because the cli takes advantage of that.
                    .canon()
                    .unwrap(),
                build_script,
                app.command.force,
                git_ref,
                app.progress.lock().unwrap().register(&language, NUM_STEPS),
                language.clone(),
                app.command.out_dir.canon().unwrap(),
                app.command.prefix.clone(),
                repo,
                app.command.target,
                ts_cli.clone(),
                cache.clone(),
            )),
            Err(err) => Err(error::Language::new(language, err)),
        };
        results.push(result);
    }

    results
}

fn get_language_coords(
    language: &str,
    defined_parsers: Option<&BTreeMap<String, ParserConfig>>,
) -> (Option<String>, Ref, TsdlResult<Url>) {
    // Attempt to find the config; defaults to None if map or key is missing
    let config = defined_parsers.and_then(|parsers| parsers.get(language));

    match config {
        Some(ParserConfig::Ref(git_ref)) => {
            (None, resolve_git_ref(git_ref), default_repo(language))
        }

        Some(ParserConfig::Full {
            build_script,
            git_ref,
            from,
        }) => {
            let url_result = match from {
                Some(url_str) => Url::parse(url_str).map_err(|e| {
                    TsdlError::context(format!("Parsing {url_str} for {language}"), e)
                }),
                None => default_repo(language),
            };

            (build_script.clone(), resolve_git_ref(git_ref), url_result)
        }

        None => (None, String::from("HEAD").into(), default_repo(language)),
    }
}

fn resolve_git_ref(git_ref: &str) -> Ref {
    let is_sha1 = git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit());

    if is_sha1 || git_ref.starts_with('v') {
        return git_ref.to_string().into();
    }

    if git_ref.split('.').all(|part| part.parse::<u32>().is_ok()) {
        format!("v{git_ref}").into()
    } else {
        git_ref.to_string().into()
    }
}

fn default_repo(language: &str) -> TsdlResult<Url> {
    let url = format!("{TSDL_FROM}{language}");
    Url::parse(&url)
        .map_err(|e| TsdlError::context(format!("Creating url {url} for {language}"), e))
}
