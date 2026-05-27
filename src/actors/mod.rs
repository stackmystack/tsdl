mod cache;
mod display;

use std::{num::NonZeroUsize, path::PathBuf, sync::Arc};

pub use cache::{CacheActor, CacheAddr};
pub use display::{DisplayActor, DisplayAddr, DisplayMessage, ProgressAddr};
use futures::{stream, StreamExt};
use tokio::sync::{mpsc, oneshot};

use tracing::{debug, info};

use crate::{
    args::TreeSitter,
    cache::{CacheKey, Source},
    error::TsdlError,
    parser::{GrammarBuild, LanguageBuild},
    shutdown, tree_sitter, TsdlResult,
};

pub trait Addr {
    type Message;

    fn name() -> &'static str;
    fn sender(&self) -> &mpsc::Sender<Self::Message>;

    #[allow(async_fn_in_trait)]
    async fn fire(&self, msg: Self::Message) {
        self.sender()
            .send(msg)
            .await
            .unwrap_or_else(|_| panic!("{}: cannot send: channel closed", Self::name()));
    }

    #[allow(async_fn_in_trait)]
    async fn request<T, F>(&self, msg: F) -> T
    where
        F: FnOnce(oneshot::Sender<T>) -> Self::Message,
    {
        let (tx, rx) = oneshot::channel();

        self.sender()
            .send(msg(tx))
            .await
            .unwrap_or_else(|_| panic!("{}: cannot send: channel closed", Self::name()));

        rx.await
            .unwrap_or_else(|_| panic!("{}: cannot recv: channel closed", Self::name()))
    }
}

pub struct Response<K, T> {
    pub kind: K,
    pub tx: oneshot::Sender<T>,
}

impl<K: std::fmt::Debug, T: std::fmt::Debug> Response<K, T> {
    /// # Panics
    ///
    /// Will panic channel is closed.
    pub fn send(self, value: T) {
        self.tx
            .send(value)
            .unwrap_or_else(|_| panic!("cannot send response: {:?}", self.kind));
    }
}

/// The entire build pipeline.
pub async fn run(
    build_dir: &PathBuf,
    cache: CacheAddr,
    display: DisplayAddr,
    jobs: NonZeroUsize,
    languages: Vec<LanguageBuild>,
    tree_sitter: &TreeSitter,
) -> TsdlResult<()> {
    let tree_sitter_ref = match tree_sitter::display_tree_sitter_ref(&tree_sitter.version) {
        Ok(git_ref) => git_ref,
        Err(err) => {
            display.shutdown(false).await;
            return Err(TsdlError::context(
                format!("Parsing tree-sitter git ref {:?}", tree_sitter.version),
                err,
            ));
        }
    };

    display.reference(tree_sitter_ref, "tree-sitter-cli").await;
    for language in &languages {
        display
            .reference(
                language.spec.git_ref.requested().clone(),
                language.name.as_arc(),
            )
            .await;
    }

    let result = run_inner(
        build_dir,
        cache,
        display.clone(),
        jobs,
        languages,
        tree_sitter,
    )
    .await;

    let interrupted = shutdown::current().and_then(|s| s.reason());

    // We need to shut down before returning the results to avoid display
    // issues with the ratatui backend.
    display.shutdown(interrupted.is_some()).await;

    // If shutdown was signalled, suppress build errors — they're expected
    // cancellation artefacts, not real failures — but still return the
    // interruption so the top-level process exits with the signal status.
    if let Some(signal) = interrupted {
        info!("pipeline shutdown signalled by {signal}, suppressing build errors");
        return Err(TsdlError::Interrupted(signal));
    }

    result
}

async fn run_inner(
    build_dir: &PathBuf,
    cache: CacheAddr,
    display: DisplayAddr,
    jobs: NonZeroUsize,
    languages: Vec<LanguageBuild>,
    tree_sitter: &TreeSitter,
) -> TsdlResult<()> {
    let prepared = tree_sitter::prepare(build_dir, display.clone(), tree_sitter).await?;
    let ts_cli = Arc::new(prepared.path);
    let languages = languages
        .into_iter()
        .map(|language| language.with_tree_sitter(prepared.tree_sitter.clone()))
        .collect::<Vec<_>>();

    let mut errors : Vec<TsdlError> =
      // 1. Source: Create a stream from the input list
      stream::iter(languages)
          // 2. Stage: Discovery
          // Transform Language -> Future<Result<Vec<Grammar>>>
          .map(|language| {
              let (cache, display, ts_cli) = (cache.clone(), display.clone(), ts_cli.clone());
              async move {
                  // We refactor `discover` to return the list instead of sending messages
                  discover_grammars(cache, display, language, ts_cli).await
              }
          })
          // Run up to `concurrency` discovery tasks at once
          .buffer_unordered(jobs.into())
          // 3. Flattening & Error Propagation
          // Turn the stream of "Lists of Grammars" into a flat stream of "Individual Grammars"
          // If discovery failed, pass the error through as an Item
          .flat_map(|discovery_result| match discovery_result {
              Ok(grammars) => stream::iter(grammars).map(Ok).left_stream(),
              Err(e) => stream::once(async { Err(e) }).right_stream(),
          })
          // 4. Stage: Build
          // Transform Result<Grammar> -> Future<Result<BuildOutcome>>
          .map(|item| async move {
              match item {
                  Ok(grammar_build) => {
                      // Execute the build logic
                      // logic formerly in GrammarActor
                      grammar_build.build().await
                  }
                  Err(e) => Err(e), // Pass upstream discovery errors down
              }
          })
          // Run up to `concurrency` build tasks at once
          .buffer_unordered(jobs.into())
          // 5. Sink: Accumulator
          // Fold the stream into the final error vector.
          // This implicitly waits for ALL tasks to finish.
          .fold(Vec::new(), |mut errors, result| {
              let cache = cache.clone();
              async move {
                  let is_shutdown = shutdown::current().is_some_and(|s| s.is_cancelled());
                  match result {
                      Ok(Some(update)) => cache.update(update).await,
                      Ok(None) => {}
                      Err(e) if is_shutdown => {
                          debug!("[pipeline:fold] shutdown, dropping error: {e}");
                      }
                      Err(e) => errors.push(e),
                  }
                  errors
              }
          })
          .await;

    if let Err(e) = cache.save().await {
        errors.push(e);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(TsdlError::Build(errors))
    }
}

// --- Helper Refactors (Moving logic out of Actor impls) ---

async fn discover_grammars(
    cache: CacheAddr,
    display: DisplayAddr,
    language: LanguageBuild,
    ts_cli: Arc<PathBuf>,
) -> TsdlResult<Vec<crate::parser::GrammarBuild>> {
    shutdown::test_delay().await;
    shutdown::check()?;
    debug!("[discover] lang={}", language.name);

    let progress = display
        .add_language(
            language.spec.git_ref.requested().clone(),
            language.name.as_arc(),
            2,
        )
        .await;

    let source = resolve_source(&cache, &language, &progress).await?;

    progress.step("scanning");
    let grammars = match language.discover_grammars().await {
        Ok(grammars) => grammars,
        Err(e) => {
            if shutdown::current().is_some_and(|s| s.is_cancelled()) {
                progress.cancel().await;
            } else {
                progress.err("scan failed").await;
            }
            return Err(e);
        }
    };
    progress.fin("done").await;

    // Map the raw discovery data into the Build struct immediately
    let mut builds = Vec::new();
    for (name, dir, hash) in grammars {
        shutdown::test_delay().await;
        shutdown::check()?;

        let key = CacheKey::new(&language.name, &name);
        let artifacts = GrammarBuild::required_artifacts_for(
            &language.output.build_dir,
            &ts_cli,
            &language.spec,
            &name,
        )?;
        let cache_decision = cache
            .needs_rebuild(
                key,
                hash.clone(),
                language.spec.clone(),
                source.clone(),
                artifacts,
            )
            .await;

        let progress = display
            .add_grammar(
                language.spec.git_ref.requested().clone(),
                language.name.as_arc(),
                name.as_arc(),
                4,
            )
            .await;

        builds.push(GrammarBuild {
            context: language.context.clone(),
            cache_decision,
            dir: dir.into(),
            hash,
            language: language.name.clone(),
            name,
            output: language.output.clone(),
            progress,
            source: source.clone(),
            spec: language.spec.clone(),
            ts_cli: ts_cli.clone(),
        });
    }

    Ok(builds)
}

async fn resolve_source(
    cache: &CacheAddr,
    language: &LanguageBuild,
    progress: &ProgressAddr,
) -> TsdlResult<Source> {
    if language.spec.git_ref.is_moving() {
        info!(
            "Resolving moving parser git ref for {}: {}",
            language.name,
            language.spec.git_ref.requested().as_str()
        );
        progress.set_outcome_built().await;
        progress.step("cloning");
        match language.checkout().await {
            Ok(checkout) => {
                info!(
                    "Resolved parser {} git ref {} to commit {}",
                    language.name,
                    language.spec.git_ref.requested().as_str(),
                    checkout.commit.as_str()
                );
                Ok(Source::moving(checkout.commit))
            }
            Err(e) => {
                if shutdown::current().is_some_and(|s| s.is_cancelled()) {
                    progress.cancel().await;
                } else {
                    progress.err("clone failed").await;
                }
                Err(e)
            }
        }
    } else {
        let has_compatible_entries = cache
            .has_compatible_entries(language.name.clone(), language.spec.clone())
            .await;
        let checkout_needed = !has_compatible_entries || !language.is_checkout_usable().await;

        if checkout_needed {
            progress.set_outcome_built().await;
            progress.step("cloning");
            if let Err(e) = language.checkout().await {
                if shutdown::current().is_some_and(|s| s.is_cancelled()) {
                    progress.cancel().await;
                } else {
                    progress.err("clone failed").await;
                }
                return Err(e);
            }
        } else {
            progress.set_outcome_cached().await;
        }

        Ok(Source::stable())
    }
}
