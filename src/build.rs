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
    consts::TSDL_FROM,
    display::{Handle, Progress, ProgressState, TICK_CHARS},
    error,
    error::TsdlError,
    git::Ref,
    parser::{build_languages, Language, NUM_STEPS},
    tree_sitter, SafeCanonicalize, TsdlResult,
};

pub fn run(app: &App) -> TsdlResult<()> {
    if app.config.show_config {
        crate::config::show(&app.config)?;
    }
    clear(app)?;
    build_impl(app)?;
    Ok(())
}

fn clear(app: &App) -> TsdlResult<()> {
    if app.config.fresh && app.config.build_dir.exists() {
        let mut progress = app
            .progress
            .lock()
            .map_err(|e| TsdlError::message(format!("Failed to acquire progress lock: {e}")))?;
        let handle = progress.register("Fresh Build", 1);
        let disp = &app.config.build_dir.display();
        fs::remove_dir_all(&app.config.build_dir)?;
        handle.fin(format!("Cleaned {disp}"));
    }
    fs::create_dir_all(&app.config.build_dir)?;
    Ok(())
}

fn build_impl(app: &App) -> TsdlResult<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(app.config.ncpus)
        .build()?;
    let _guard = rt.enter();
    rt.spawn(update_screen(app.progress.clone()));
    let ts_cli = rt.block_on(tree_sitter::prepare(&app.config, app.progress.clone()))?;

    let languages = collect_languages(
        app,
        ts_cli,
        app.config.languages.as_ref(),
        app.config.parsers.as_ref(),
    )?;
    create_dir_all(&app.config.out_dir)?;
    rt.block_on(build_languages(languages))
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
) -> Result<Vec<Language>, error::LanguageCollection> {
    let (res, errs) = unique_languages(app, ts_cli, requested_languages, defined_parsers);
    if errs.is_empty() {
        Ok(res.into_iter().map(Result::unwrap).collect())
    } else {
        Err(error::LanguageCollection {
            related: errs.into_iter().map(Result::unwrap_err).collect(),
        })
    }
}

type Languages = (
    Vec<Result<Language, error::Language>>,
    Vec<Result<Language, error::Language>>,
);

fn unique_languages(
    app: &App,
    ts_cli: PathBuf,
    requested_languages: Option<&Vec<String>>,
    defined_parsers: Option<&BTreeMap<String, ParserConfig>>,
) -> Languages {
    let ts_cli = Arc::new(ts_cli);
    let final_languages = match requested_languages {
        Some(langs) if !langs.is_empty() => langs.clone(),
        _ => defined_parsers
            .map(|parsers| parsers.keys().cloned().collect())
            .unwrap_or_default(),
    };
    final_languages
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|language| {
            let (build_script, git_ref, url) = get_language_coords(&language, defined_parsers);
            url.map(|repo| {
                Language::new(
                    app.config
                        .build_dir
                        .join(format!("tree-sitter-{}", &language)) // make sure it follows this format because the cli takes advantage of that.
                        .canon()
                        .unwrap(),
                    build_script,
                    git_ref,
                    app.progress.lock().unwrap().register(&language, NUM_STEPS),
                    language.clone(),
                    app.config.out_dir.canon().unwrap(),
                    app.config.prefix.clone(),
                    repo,
                    app.config.target,
                    ts_cli.clone(),
                )
            })
            .map_err(|err| error::Language::new(language, err))
        })
        .partition(Result::is_ok)
}

fn get_language_coords(
    language: &str,
    defined_parsers: Option<&BTreeMap<String, ParserConfig>>,
) -> (Option<String>, Ref, TsdlResult<Url>) {
    match defined_parsers.as_ref().and_then(|p| p.get(language)) {
        Some(ParserConfig::Ref(git_ref)) => {
            (None, resolve_git_ref(git_ref), default_repo(language))
        }
        Some(ParserConfig::Full {
            build_script,
            git_ref,
            from,
        }) => (
            build_script.clone(),
            resolve_git_ref(git_ref),
            from.as_ref().map_or_else(
                || default_repo(language),
                |f| {
                    Url::parse(f)
                        .map_err(|e| TsdlError::context(format!("Parsing {f} for {language}"), e))
                },
            ),
        ),
        _ => (None, String::from("HEAD").into(), default_repo(language)),
    }
}

fn resolve_git_ref(git_ref: &str) -> Ref {
    Some(git_ref)
        .filter(|f| f.len() != 40 && !f.starts_with('v'))
        .and_then(|f| {
            let versions = f.split('.').collect::<Vec<_>>();
            if !versions.is_empty() && versions.iter().all(|f| f.parse::<u32>().is_ok()) {
                Some(format!("v{f}").into())
            } else {
                None
            }
        })
        .unwrap_or_else(|| git_ref.to_string().into())
}

fn default_repo(language: &str) -> TsdlResult<Url> {
    let url = format!("{TSDL_FROM}{language}");
    Url::parse(&url)
        .map_err(|e| TsdlError::context(format!("Creating url {url} for {language}"), e))
}
