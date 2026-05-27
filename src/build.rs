use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tracing::info;
use url::Url;

use crate::{
    actors, app, args, cache, consts, error, format_duration, lock, parser, prompt_user, shutdown,
    SafeCanonicalize, TsdlResult,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub build_script: Option<String>,
    pub git_ref: parser::Ref,
    pub prefix: String,
    pub repo: Url,
    pub target: args::Target,
    pub tree_sitter: args::TreeSitter,
}

#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub build_dir: Arc<PathBuf>,
    pub out_dir: Arc<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub overwrite_output: bool,
}

pub fn run(app: &app::App) -> TsdlResult<()> {
    if app.command.show_config {
        crate::config::show(&app.command)?;
    }

    let lock = lock::Lock::new(&app.command.build_dir);
    let guard = acquire_lock(&lock, Duration::from_secs(app.command.unlock_timeout))?;

    clear(app, &guard)?;
    ignite(app)?;
    Ok(())
}

fn acquire_lock(lock: &lock::Lock, unlock_timeout: Duration) -> TsdlResult<lock::Guard> {
    // Loop because the lock owner may exit naturally between the prompt and
    // SIGTERM, or another process may replace it.  Each iteration re-checks
    // the lock status and presents the current owner to the user.
    loop {
        match lock.try_acquire()? {
            lock::Status::Acquired(guard) => return Ok(guard),

            lock::Status::Cyclic => {
                info!("lock::Lock already held by this process (cyclic).");
                return Err(error::TsdlError::message("1+ lock acquisition"));
            }

            lock::Status::LockedBy(owner) => match handle_locked_by(lock, &owner, unlock_timeout) {
                Ok(guard) => return Ok(guard),
                Err(ref err) if err.is_retryable() => {
                    info!("{err}; re-checking lock status...");
                    // continue to the next loop iteration
                }
                Err(err) => return Err(err.into()),
            },

            lock::Status::Unknown { pid, reason } => {
                if let Some(pid) = pid {
                    info!("Build directory is locked by PID {pid}, but tsdl could not inspect it: {reason}");
                } else {
                    info!("Build directory is locked, but tsdl could not identify the owner: {reason}");
                }
                return Err(error::TsdlError::message(format!(
                    "Could not identify build lock owner: {reason}"
                )));
            }
        }
    }
}

fn handle_locked_by(
    lock: &lock::Lock,
    owner: &lock::Owner,
    unlock_timeout: Duration,
) -> Result<lock::Guard, lock::TakeoverError> {
    info!("Build directory is locked by another process:");
    info!("{owner}");
    eprintln!(
        "If you continue, tsdl will send SIGTERM to PID {} and wait up to {} \
         (--unlock-timeout {}) for the build lock to be released.",
        owner.pid,
        format_duration(unlock_timeout),
        unlock_timeout.as_secs()
    );

    if !prompt_user("Terminate this process and continue?", false)? {
        return Err(error::TsdlError::message("lock::Lock acquisition cancelled by user").into());
    }

    lock.terminate_owner(owner)?;
    info!(
        "Sent SIGTERM to PID {}. Waiting for the build lock to be released...",
        owner.pid
    );
    lock.wait_for_release(owner, unlock_timeout)
}

fn clear(app: &app::App, guard: &lock::Guard) -> TsdlResult<()> {
    if app.command.fresh && app.command.build_dir.exists() {
        guard.clear_directory(std::slice::from_ref(&app.log_path))?;
    }

    fs::create_dir_all(&app.command.build_dir)?;

    Ok(())
}

fn collect_languages(
    app: &app::App,
) -> Result<Vec<parser::LanguageBuild>, error::LanguageCollection> {
    let results = unique_languages(app);
    let (ok, err): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);

    if err.is_empty() {
        Ok(ok.into_iter().map(Result::unwrap).collect())
    } else {
        Err(error::LanguageCollection {
            related: err.into_iter().map(Result::unwrap_err).collect(),
        })
    }
}

fn default_repo(language: &str) -> TsdlResult<Url> {
    let url = format!("{}{language}", consts::TSDL_FROM);
    Url::parse(&url)
        .map_err(|e| error::TsdlError::context(format!("Creating url {url} for {language}"), e))
}

fn get_language_coords(
    language: &str,
    defined_parsers: Option<&BTreeMap<String, args::ParserConfig>>,
) -> TsdlResult<(Option<String>, parser::Ref, Url)> {
    let config = defined_parsers.and_then(|parsers| parsers.get(language));

    match config {
        Some(args::ParserConfig::Ref(git_ref)) => Ok((
            None,
            parser::Ref::parse(git_ref).map_err(|e| {
                error::TsdlError::context(format!("Parsing git ref {git_ref:?} for {language}"), e)
            })?,
            default_repo(language)?,
        )),

        Some(args::ParserConfig::Full {
            build_script,
            git_ref,
            from,
        }) => {
            let repo = match from {
                Some(url_str) => Url::parse(url_str).map_err(|e| {
                    error::TsdlError::context(format!("Parsing {url_str} for {language}"), e)
                })?,
                None => default_repo(language)?,
            };

            Ok((
                build_script.clone(),
                parser::Ref::parse(git_ref).map_err(|e| {
                    error::TsdlError::context(
                        format!("Parsing git ref {git_ref:?} for {language}"),
                        e,
                    )
                })?,
                repo,
            ))
        }

        None => Ok((None, parser::Ref::head(), default_repo(language)?)),
    }
}

fn ignite(app: &app::App) -> TsdlResult<()> {
    fs::create_dir_all(&app.command.out_dir)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let guard = rt.enter();

    let db = cache::Db::load(&app.command.build_dir)?;
    let languages = collect_languages(app)?;

    let result = rt.block_on(async move {
        let shutdown = shutdown::Handle::new();
        let _signals = shutdown.spawn_signal_listener()?;
        let cache = actors::CacheActor::spawn(db, app.command.force);
        let build_dir: Arc<PathBuf> = app.command.build_dir.canon()?.into();
        let out_dir: Arc<PathBuf> = app.command.out_dir.canon()?.into();
        let display = actors::DisplayActor::spawn(app.progress_mode, build_dir, out_dir);

        shutdown::scope(shutdown, async move {
            actors::run(
                &app.command.build_dir,
                cache,
                display,
                app.command.jobs,
                languages,
                &app.command.tree_sitter,
            )
            .await
        })
        .await?;

        Ok(())
    });

    drop(guard);

    result
}

fn unique_languages(app: &app::App) -> Vec<Result<parser::LanguageBuild, error::Language>> {
    let requested_languages = &app.command.languages;
    let defined_parsers = app.command.parsers.as_ref();

    let final_languages = match requested_languages {
        Some(langs) if !langs.is_empty() => langs.clone(),
        _ => defined_parsers
            .map(|parsers| parsers.keys().cloned().collect())
            .unwrap_or_default(),
    };

    let unique = final_languages.into_iter().collect::<BTreeSet<_>>();
    let mut results = Vec::with_capacity(unique.len());

    for language in unique {
        let result = match get_language_coords(&language, defined_parsers) {
            Ok((build_script, git_ref, repo)) => Ok(parser::LanguageBuild::new(
                Context {
                    overwrite_output: app.command.force,
                },
                Arc::new(Spec {
                    build_script,
                    git_ref,
                    repo,
                    tree_sitter: app.command.tree_sitter.clone(),
                    prefix: app.command.prefix.clone(),
                    target: app.command.target,
                }),
                parser::LanguageName::from(language.clone()),
                OutputConfig {
                    build_dir: app
                        .command
                        .build_dir
                        .join(format!("tree-sitter-{}", &language))
                        .canon()
                        .expect("Build dir canonicalization failed")
                        .into(),
                    out_dir: app
                        .command
                        .out_dir
                        .canon()
                        .expect("Out dir canonicalization failed")
                        .into(),
                },
            )),
            Err(err) => Err(error::Language::new(language, err)),
        };
        results.push(result);
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{args::BuildCommand, config::BuildProvenance, display::Mode};

    fn app_with_languages(languages: &[&str]) -> app::App {
        let command = BuildCommand {
            languages: Some(
                languages
                    .iter()
                    .map(|language| (*language).to_string())
                    .collect(),
            ),
            ..BuildCommand::default()
        };

        app::App {
            subcommand: crate::args::Command::Build,
            command,
            config_path: PathBuf::from("parsers.toml"),
            log_path: PathBuf::from("tmp/log"),
            progress_mode: Mode::Plain,
            provenance: BuildProvenance::default(),
            verbose: clap_verbosity_flag::Verbosity::default(),
            _logging: None,
        }
    }

    #[test]
    fn unique_languages_sorts_and_deduplicates_requested_languages() {
        let app = app_with_languages(&["rust", "json", "ruby", "json", "rust"]);

        let languages = unique_languages(&app)
            .into_iter()
            .map(|language| language.unwrap().name.to_string())
            .collect::<Vec<_>>();

        assert_eq!(languages, vec!["json", "ruby", "rust"]);
    }
}
