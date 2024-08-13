use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, create_dir_all},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use miette::{miette, Context, IntoDiagnostic, Result};
use tokio::time;
use tracing::error;
use url::Url;

use crate::{
    args::{BuildCommand, ParserConfig},
    config,
    consts::TSDL_FROM,
    display::{Handle, Progress, ProgressState, TICK_CHARS},
    error,
    git::Ref,
    parser::{build_languages, Language, NUM_STEPS},
    tree_sitter, SafeCanonicalize,
};

pub fn run(command: &BuildCommand, mut progress: Progress) -> Result<()> {
    if command.show_config {
        config::show(command)?;
    }
    clear(command, &mut progress)?;
    build(command, progress)?;
    Ok(())
}

fn clear(command: &BuildCommand, progress: &mut Progress) -> Result<()> {
    if command.fresh && command.build_dir.exists() {
        let handle = progress.register("Fresh Build", 1);
        let disp = &command.build_dir.display();
        fs::remove_dir_all(&command.build_dir)
            .into_diagnostic()
            .wrap_err(format!("Removing the build_dir {disp} for a fresh build"))?;
        handle.fin(format!("Cleaned {disp}"));
    }
    fs::create_dir_all(&command.build_dir)
        .into_diagnostic()
        .wrap_err("Creating the build dir")?;
    Ok(())
}

fn build(command: &BuildCommand, progress: Progress) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(command.ncpus)
        .build();
    if let Err(ref err) = rt {
        error!("Failed to initialize tokio.");
        error!("{err}");
        return Err(miette!("Failed to spawn the tokio runtime"));
    }
    let rt = rt.unwrap();
    let _guard = rt.enter();
    let screen = Arc::new(Mutex::new(progress));
    rt.spawn(update_screen(screen.clone()));
    let ts_cli = rt
        .block_on(tree_sitter::prepare(command, screen.clone()))
        .wrap_err("Preparing tree-sitter")?;
    let languages = languages(
        ts_cli,
        screen,
        &command.languages,
        &command.parsers,
        command.build_dir.clone(),
        command.out_dir.clone(),
        &command.prefix,
    )
    .unwrap();
    create_dir_all(&command.out_dir)
        .into_diagnostic()
        .wrap_err(format!(
            "Creating output dir {}",
            &command.out_dir.display()
        ))
        .unwrap();
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

fn languages(
    ts_cli: PathBuf,
    progress: Arc<Mutex<Progress>>,
    requested_languages: &Option<Vec<String>>,
    defined_parsers: &Option<BTreeMap<String, ParserConfig>>,
    build_dir: PathBuf,
    out_dir: PathBuf,
    prefix: &str,
) -> Result<Vec<Language>, error::LanguageCollection> {
    let (res, errs) = unique_languages(
        ts_cli,
        build_dir,
        out_dir,
        prefix,
        requested_languages,
        defined_parsers,
        progress,
    );
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

#[allow(clippy::needless_pass_by_value)]
fn unique_languages(
    ts_cli: PathBuf,
    build_dir: PathBuf,
    out_dir: PathBuf,
    prefix: &str,
    requested_languages: &Option<Vec<String>>,
    defined_parsers: &Option<BTreeMap<String, ParserConfig>>,
    progress: Arc<Mutex<Progress>>,
) -> Languages {
    let ts_cli = Arc::new(ts_cli);
    let final_languages = requested_languages
        .clone()
        .filter(|arr| !arr.is_empty())
        .or_else(|| {
            defined_parsers
                .as_ref()
                .map(|map| map.keys().cloned().collect())
        })
        .unwrap_or_default();
    final_languages
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|language| {
            let (build_script, git_ref, url) = coords(&language, defined_parsers);
            url.map(|repo| {
                Language::new(
                    build_dir.join(&language).canon().unwrap(),
                    build_script,
                    git_ref,
                    progress.lock().unwrap().register(&language, NUM_STEPS),
                    language.clone(),
                    out_dir.canon().unwrap(),
                    prefix.into(),
                    repo,
                    ts_cli.clone(),
                )
            })
            .map_err(|err| error::Language {
                name: language,
                source: err.into(),
            })
        })
        .partition(Result::is_ok)
}

fn coords(
    language: &str,
    defined_parsers: &Option<BTreeMap<String, ParserConfig>>,
) -> (Option<String>, Ref, Result<Url>) {
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
                        .into_diagnostic()
                        .wrap_err(format!("Parsing {f} for {language}"))
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

fn default_repo(language: &str) -> Result<Url> {
    let url = format!("{TSDL_FROM}{language}");
    Url::parse(&url)
        .into_diagnostic()
        .wrap_err(format!("Creating url {url} for {language}"))
}
