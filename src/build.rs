use std::fmt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    result::Result as StdResult,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tracing::info;
use url::Url;

use crate::{
    absolute_normalize, actors, app, args, cache, consts, format_duration, lock, parser,
    prompt_user, shutdown, Error, Result, ResultExt, SafeCanonicalize,
};

// ============================================================
// Structs
// ============================================================

/// The root build directory — anchor for all derived paths.
#[derive(Clone, Debug)]
pub struct BuildDir(PathBuf);

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub overwrite_output: bool,
}

#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub build_dir: PathBuf,
    pub out_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub build_script: Option<String>,
    pub git_ref: parser::Ref,
    pub prefix: String,
    pub repo: Url,
    pub target: args::Target,
    pub tree_sitter: args::TreeSitter,
}

// ============================================================
// Free functions
// ============================================================

fn acquire_lock(lock: &lock::Lock, unlock_timeout: Duration) -> Result<lock::Guard> {
    // Loop because the lock owner may exit naturally between the prompt and
    // SIGTERM, or another process may replace it.  Each iteration re-checks
    // the lock status and presents the current owner to the user.
    loop {
        match lock.try_acquire()? {
            lock::Status::Acquired(guard) => return Ok(guard),

            lock::Status::Cyclic => {
                info!("lock::Lock already held by this process (cyclic).");
                return Err(Error::Message {
                    message: "1+ lock acquisition".into(),
                });
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
                return Err(Error::Message {
                    message: format!("Could not identify build lock owner: {reason}"),
                });
            }
        }
    }
}

fn clear(
    build_dir: &BuildDir,
    guard: &lock::Guard,
    fresh: bool,
    log_path: Option<&Path>,
) -> Result<()> {
    if fresh && build_dir.as_path().exists() {
        let protected_files = log_path
            .map(Path::to_path_buf)
            .into_iter()
            .collect::<Vec<_>>();
        guard.clear_directory(&protected_files)?;
    }

    fs::create_dir_all(build_dir.as_path())?;

    Ok(())
}

fn collect_languages(
    command: &args::BuildCommand,
    build_dir: &BuildDir,
) -> Result<Vec<parser::LanguageBuild>> {
    let results = unique_languages(command, build_dir);
    let (ok, err): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);

    if err.is_empty() {
        Ok(ok.into_iter().map(Result::unwrap).collect())
    } else {
        Err(Error::LanguageCollection {
            related: err.into_iter().map(Result::unwrap_err).collect(),
        })
    }
}

fn default_repo(language: &str) -> Result<Url> {
    use consts::FROM;

    let url = format!("{FROM}{language}");
    Url::parse(&url).with_context(|| format!("Creating url {url} for {language}"))
}

fn get_language_coords(
    language: &str,
    defined_parsers: Option<&BTreeMap<String, args::ParserConfig>>,
) -> Result<(Url, parser::Ref, Option<String>)> {
    let config = defined_parsers.and_then(|parsers| parsers.get(language));

    match config {
        Some(args::ParserConfig::Ref(git_ref)) => Ok((
            default_repo(language)?,
            parser::Ref::parse(git_ref)
                .with_context(|| format!("Parsing git ref {git_ref:?} for {language}"))?,
            None,
        )),

        Some(args::ParserConfig::Full {
            build_script,
            git_ref,
            from,
        }) => {
            let repo = match from {
                Some(url_str) => Url::parse(url_str)
                    .with_context(|| format!("Parsing {url_str} for {language}"))?,
                None => default_repo(language)?,
            };

            Ok((
                repo,
                parser::Ref::parse(git_ref)
                    .with_context(|| format!("Parsing git ref {git_ref:?} for {language}"))?,
                build_script.clone(),
            ))
        }

        None => Ok((default_repo(language)?, parser::Ref::head(), None)),
    }
}

fn handle_locked_by(
    lock: &lock::Lock,
    owner: &lock::Owner,
    unlock_timeout: Duration,
) -> StdResult<lock::Guard, lock::TakeoverError> {
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
        return Err(Error::Message {
            message: "lock::Lock acquisition cancelled by user".into(),
        }
        .into());
    }

    lock.terminate_owner(owner)?;
    info!(
        "Sent SIGTERM to PID {}. Waiting for the build lock to be released...",
        owner.pid
    );
    lock.wait_for_release(owner, unlock_timeout)
}

fn ignite(command: &args::BuildCommand, app: &app::App, build_dir: &BuildDir) -> Result<()> {
    fs::create_dir_all(&command.out_dir)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let guard = rt.enter();

    let cache_store = cache::Store::new(build_dir);
    let db = cache_store.load()?;
    let languages = collect_languages(command, build_dir)?;

    let result = rt.block_on(async move {
        let shutdown = shutdown::Handle::new();
        let _signals = shutdown.spawn_signal_listener()?;
        let cache = actors::CacheActor::spawn(db, command.force, cache_store);
        let display_build_dir = build_dir.as_path().to_path_buf();
        let display_out_dir = command.out_dir.canon()?;
        let display =
            actors::DisplayActor::spawn(display_build_dir, app.progress_mode, display_out_dir);

        shutdown::scope(shutdown, async move {
            actors::run(
                build_dir.as_path(),
                cache,
                display,
                command.jobs,
                languages,
                &command.tree_sitter,
            )
            .await
        })
        .await?;

        Ok(())
    });

    drop(guard);

    result
}

pub fn run(command: &args::BuildCommand, app: &app::App) -> Result<()> {
    if command.show_config {
        crate::config::show(command)?;
    }

    let build_dir = BuildDir::new(&command.build_dir)?;
    let lock = lock::Lock::new(&build_dir);
    let guard = acquire_lock(&lock, Duration::from_secs(command.unlock_timeout))?;

    clear(&build_dir, &guard, command.fresh, app.logging.path())?;
    ignite(command, app, &build_dir)?;
    Ok(())
}

fn unique_languages(
    command: &args::BuildCommand,
    build_dir: &BuildDir,
) -> Vec<Result<parser::LanguageBuild>> {
    let requested_languages = &command.languages;
    let defined_parsers = command.parsers.as_ref();

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
            Ok((repo, git_ref, build_script)) => Ok(parser::LanguageBuild::new(
                Context {
                    overwrite_output: command.force,
                },
                parser::LanguageName::from(language.clone()),
                OutputConfig {
                    build_dir: build_dir
                        .checkout_dir(&language)
                        .canon()
                        .expect("Build dir canonicalization failed"),
                    out_dir: command
                        .out_dir
                        .canon()
                        .expect("Out dir canonicalization failed"),
                },
                Arc::new(Spec {
                    build_script,
                    git_ref,
                    repo,
                    tree_sitter: command.tree_sitter.clone(),
                    prefix: command.prefix.clone(),
                    target: command.target,
                }),
            )),
            Err(err) => Err(Error::Language {
                name: language,
                source: err.into(),
            }),
        };
        results.push(result);
    }

    results
}

// ============================================================
// Impls
// ============================================================

impl AsRef<Path> for BuildDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl BuildDir {
    /// The absolute, normalised path as a `&Path`.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Path to the build cache TOML file inside this build directory.
    #[must_use]
    pub fn cache_file(&self) -> PathBuf {
        self.0.join(consts::CACHE_FILE)
    }

    /// Per-language checkout directory: `<build_dir>/tree-sitter-<language>`.
    #[must_use]
    pub fn checkout_dir(&self, language: &str) -> PathBuf {
        self.0.join(format!("tree-sitter-{language}"))
    }

    /// Path to the PID lock file inside this build directory.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.0.join(consts::LOCK_FILE)
    }

    /// Path to the default log file inside this build directory.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.0.join(consts::LOG_FILE)
    }

    /// Create a `BuildDir`, resolving relative paths against the current
    /// working directory and normalising `.` / `..` components.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = absolute_normalize(&path.into())?;
        Ok(Self(path))
    }
}

impl fmt::Display for BuildDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::BuildCommand;

    fn command_with_languages(languages: &[&str]) -> BuildCommand {
        BuildCommand {
            languages: Some(
                languages
                    .iter()
                    .map(|language| (*language).to_string())
                    .collect(),
            ),
            ..BuildCommand::default()
        }
    }

    #[test]
    fn unique_languages_sorts_and_deduplicates_requested_languages() {
        let command = command_with_languages(&["rust", "json", "ruby", "json", "rust"]);
        let build_dir = BuildDir::new("test-build").unwrap();

        let languages = unique_languages(&command, &build_dir)
            .into_iter()
            .map(|language| language.unwrap().name.to_string())
            .collect::<Vec<_>>();

        assert_eq!(languages, vec!["json", "ruby", "rust"]);
    }
}
