use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tracing::info;
use url::Url;

use crate::{
    actors::{self, CacheActor, DisplayActor},
    app::App,
    args::{ParserConfig, Target, TreeSitter},
    cache::Db,
    consts::TSDL_FROM,
    error::{self, TsdlError},
    format_duration,
    git::GitRef,
    lock::{Lock, LockGuard, LockOwner, LockStatus},
    parser::LanguageBuild,
    prompt_user,
    shutdown::{self, Shutdown},
    SafeCanonicalize, TsdlResult,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildSpec {
    pub build_script: Option<String>,
    pub git_ref: GitRef,
    pub prefix: String,
    pub repo: Url,
    pub target: Target,
    pub tree_sitter: TreeSitter,
}

#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub build_dir: Arc<PathBuf>,
    pub out_dir: Arc<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuildContext {
    pub cache_hit: bool,
    pub force: bool,
}

pub fn run(app: &mut App) -> TsdlResult<()> {
    if app.command.show_config {
        crate::config::show(&app.command)?;
    }

    let lock = Lock::new(&app.command.build_dir);
    let _guard = acquire_lock(&lock, Duration::from_secs(app.command.unlock_timeout))?;

    clear(app)?;
    ignite(app)?;
    Ok(())
}

fn acquire_lock(lock: &Lock, unlock_timeout: Duration) -> TsdlResult<LockGuard> {
    // Loop because the lock owner may exit naturally between the prompt and
    // SIGTERM, or another process may replace it.  Each iteration re-checks
    // the lock status and presents the current owner to the user.
    loop {
        match lock.try_acquire()? {
            LockStatus::Acquired(guard) => return Ok(guard),

            LockStatus::Cyclic => {
                info!("Lock already held by this process (cyclic).");
                return Err(TsdlError::message("1+ lock acquisition"));
            }

            LockStatus::LockedBy(owner) => match handle_locked_by(lock, &owner, unlock_timeout) {
                Ok(guard) => return Ok(guard),
                Err(ref e) if is_retryable_lock_error(e) => {
                    info!("Lock owner changed; re-checking lock status...");
                    // continue to the next loop iteration
                }
                Err(e) => return Err(e),
            },

            LockStatus::Unknown { pid, reason } => {
                match pid {
                    Some(pid) => info!("Build directory is locked by PID {pid}, but tsdl could not inspect it: {reason}"),
                    None => info!("Build directory is locked, but tsdl could not identify the owner: {reason}"),
                }
                return Err(TsdlError::message(format!(
                    "Could not identify build lock owner: {reason}"
                )));
            }
        }
    }
}

/// Returns `true` when the error indicates that the lock owner disappeared
/// between inspection and the takeover attempt.  In that case the caller
/// should re-check lock status instead of failing.
fn is_retryable_lock_error(err: &TsdlError) -> bool {
    let msg = err.to_string();
    msg.contains("no longer running") || msg.contains("no longer matches the process")
}

fn handle_locked_by(
    lock: &Lock,
    owner: &LockOwner,
    unlock_timeout: Duration,
) -> TsdlResult<crate::lock::LockGuard> {
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
        return Err(TsdlError::message("Lock acquisition cancelled by user"));
    }

    lock.terminate_owner(owner)?;
    info!(
        "Sent SIGTERM to PID {}. Waiting for the build lock to be released...",
        owner.pid
    );
    lock.wait_for_release(owner, unlock_timeout)
}

fn clear(app: &mut App) -> TsdlResult<()> {
    if app.command.fresh && app.command.build_dir.exists() {
        fs::remove_dir_all(&app.command.build_dir)?;
        info!("Cleaned {}", app.command.build_dir.display());
    }

    fs::create_dir_all(&app.command.build_dir)?;

    Ok(())
}

fn collect_languages(app: &App) -> Result<Vec<LanguageBuild>, error::LanguageCollection> {
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
    let url = format!("{TSDL_FROM}{language}");
    Url::parse(&url)
        .map_err(|e| TsdlError::context(format!("Creating url {url} for {language}"), e))
}

fn get_language_coords(
    language: &str,
    defined_parsers: Option<&BTreeMap<String, ParserConfig>>,
) -> (Option<String>, GitRef, TsdlResult<Url>) {
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

        None => (None, GitRef::from("HEAD"), default_repo(language)),
    }
}

fn ignite(app: &App) -> TsdlResult<()> {
    fs::create_dir_all(&app.command.out_dir)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let guard = rt.enter();

    let db = Db::load(&app.command.build_dir)?;
    let languages = collect_languages(app)?;

    let result = rt.block_on(async move {
        let shutdown = Shutdown::new();
        let _signals = shutdown.spawn_signal_listener()?;
        let cache = CacheActor::spawn(db, app.command.force);
        let build_dir: Arc<PathBuf> = app.command.build_dir.canon()?.into();
        let out_dir: Arc<PathBuf> = app.command.out_dir.canon()?.into();
        let display = DisplayActor::spawn(app.progress_mode, build_dir, out_dir);

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

fn resolve_git_ref(git_ref: &str) -> GitRef {
    let is_sha1 = git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit());

    if is_sha1 || git_ref.starts_with('v') {
        return GitRef::from(git_ref);
    }

    if git_ref.split('.').all(|part| part.parse::<u32>().is_ok()) {
        GitRef::from(format!("v{git_ref}"))
    } else {
        GitRef::from(git_ref)
    }
}

fn unique_languages(app: &App) -> Vec<Result<LanguageBuild, error::Language>> {
    let requested_languages = &app.command.languages;
    let defined_parsers = app.command.parsers.as_ref();

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
            Ok(repo) => Ok(LanguageBuild::new(
                BuildContext {
                    force: app.command.force || app.command.fresh,
                    cache_hit: false,
                },
                Arc::new(BuildSpec {
                    build_script,
                    git_ref,
                    repo,
                    tree_sitter: app.command.tree_sitter.clone(),
                    prefix: app.command.prefix.clone(),
                    target: app.command.target,
                }),
                language.clone().into(),
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
