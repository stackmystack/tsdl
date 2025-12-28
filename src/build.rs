use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, create_dir_all},
    path::PathBuf,
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use tokio::time;
use url::Url;

use crate::{
    actors::{self, CacheActor, DisplayActor},
    app::App,
    args::{ParserConfig, Target, TreeSitter},
    cache::Db,
    consts::TSDL_FROM,
    display::{self, Progress, ProgressBar, TICK_CHARS},
    error::{self, TsdlError},
    git::GitRef,
    lock::{Lock, LockStatus},
    parser::LanguageBuild,
    prompt_user, SafeCanonicalize, TsdlResult,
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
    pub progress: Option<display::ProgressBar>,
}

impl BuildContext {
    pub fn err(&self, msg: &str) {
        if let Some(ref progress) = self.progress {
            progress.err(msg);
        }
    }

    pub fn fin(&self, msg: &str) {
        if let Some(ref progress) = self.progress {
            progress.fin(msg);
        }
    }

    pub fn msg(&self, msg: &str) {
        if let Some(ref progress) = self.progress {
            progress.msg(msg);
        }
    }

    pub fn step(&self, msg: &str) {
        if let Some(ref progress) = self.progress {
            progress.step(msg);
        }
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        self.progress.as_ref().is_none_or(ProgressBar::is_done)
    }

    pub fn start(&mut self, msg: &str) {
        if let Some(ref mut progress) = self.progress {
            progress.step(msg);
        }
    }

    pub fn tick(&self) {
        if let Some(ref progress) = self.progress {
            progress.tick();
        }
    }
}

pub fn run(app: &App) -> TsdlResult<()> {
    if app.command.show_config {
        crate::config::show(&app.command)?;
    }

    // Initialize the manager first with the build directory
    let lock = Lock::new(&app.command.build_dir);

    if app.command.unlock {
        lock.force_unlock()?;
    }

    // Check lock status before clearing anything

    let _guard = match lock.try_acquire()? {
        LockStatus::Acquired(lock) => lock,

        LockStatus::Cyclic => {
            eprintln!("Lock already held by this process. This should not happen.");
            return Err(TsdlError::message("1+ lock acquisition"));
        }

        LockStatus::LockedBy { pid, exe } => {
            eprintln!("Lock owned by different process: PID {pid} ({exe})");
            if prompt_user("Proceed anyway?", false)? {
                // Use the manager instance to force acquire
                lock.force_acquire()?
            } else {
                return Err(TsdlError::message("Lock acquisition cancelled by user"));
            }
        }

        LockStatus::Stale(pid) => {
            eprintln!("Found stale lock from PID {pid} (process no longer exists)");
            if prompt_user("Take over lock?", true)? {
                lock.force_acquire()?
            } else {
                return Err(TsdlError::message("Lock acquisition cancelled by user"));
            }
        }

        LockStatus::Unknown { pid, reason } => {
            eprintln!("Could not verify lock owner PID {pid}: {reason}",);
            if prompt_user("Take over lock?", false)? {
                lock.force_acquire()?
            } else {
                return Err(TsdlError::message("Lock acquisition cancelled by user"));
            }
        }
    };

    clear(app)?;
    ignite(app)?;
    Ok(())
}

fn clear(app: &App) -> TsdlResult<()> {
    if app.command.fresh && app.command.build_dir.exists() {
        let mut progress = app
            .progress
            .lock()
            .map_err(|e| TsdlError::message(format!("Failed to acquire progress lock: {e}")))?;
        let bar = progress.register("Fresh Build".into(), "".into(), 1);
        fs::remove_dir_all(&app.command.build_dir)?;
        bar.fin(format!("Cleaned {}", app.command.build_dir.display()));
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

        None => (None, GitRef::from("HEAD"), default_repo(language)),
    }
}

fn ignite(app: &App) -> TsdlResult<()> {
    create_dir_all(&app.command.out_dir)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let guard = rt.enter();

    let db = Db::load(&app.command.build_dir)?;
    let languages = collect_languages(app)?;
    let progress = app.progress.clone();

    let result = rt.block_on(async move {
        let cache = CacheActor::spawn(db, app.command.force);
        let display = DisplayActor::spawn(progress.clone());

        tokio::spawn(async { update_screen(progress).await });

        actors::run(
            &app.command.build_dir,
            cache,
            display,
            app.command.jobs,
            languages,
            &app.command.tree_sitter,
        )
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
                    progress: None, // Progress is handled by DisplayActor
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

async fn update_screen(progress: Arc<std::sync::Mutex<Progress>>) {
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
