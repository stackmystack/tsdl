mod cache;
mod display;

use std::{path::PathBuf, sync::Arc};

pub use cache::{CacheActor, CacheAddr};
pub use display::{DisplayActor, DisplayAddr, DisplayMessage, ProgressAddr};
use futures::{stream, StreamExt};
use tokio::sync::{mpsc, oneshot};

use crate::{
    args::TreeSitter,
    error::TsdlError,
    parser::{GrammarBuild, LanguageBuild},
    tree_sitter, TsdlResult,
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
    jobs: usize,
    languages: Vec<LanguageBuild>,
    tree_sitter: &TreeSitter,
) -> TsdlResult<()> {
    let ts_cli = Arc::new(tree_sitter::prepare(build_dir, display.clone(), tree_sitter).await?);

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
          .buffer_unordered(jobs)
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
          .buffer_unordered(jobs)
          // 5. Sink: Accumulator
          // Fold the stream into the final error vector.
          // This implicitly waits for ALL tasks to finish.
          .fold(Vec::new(), |mut errors, result| {
              let cache = cache.clone();
              async move {
                  match result {
                      Ok(Some(update)) => cache.update(update).await, // Side-effect: Cache Update
                      Ok(None) => {}                                  // Cache hit
                      Err(e) => errors.push(e),                       // Accumulate Error
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
    let progress = display
        .add_language(language.spec.git_ref.clone(), language.name.clone(), 3)
        .await?;

    // ... (Clone logic same as original) ...
    if cache
        .needs_clone(language.name.clone(), language.spec.clone())
        .await
    {
        progress.step("cloning");
        language.clone().await?;
    }

    progress.step("scanning");
    let grammars = language.discover_grammars().await?;

    // Map the raw discovery data into the Build struct immediately
    let mut builds = Vec::new();
    for (name, dir, hash) in grammars {
        let key = format!("{}/{}", language.name, name);
        let entry = cache.get(key).await;
        let name_arc: std::sync::Arc<str> = name.into();

        let progress = display
            .add_grammar(
                language.spec.git_ref.clone(),
                name_arc.clone(),
                language.name.clone(),
                4,
            )
            .await?;

        builds.push(GrammarBuild {
            context: language.context.clone(),
            dir: dir.into(),
            entry,
            hash: hash.into(),
            language: language.name.clone(),
            name: name_arc,
            output: language.output.clone(),
            progress,
            spec: language.spec.clone(),
            ts_cli: ts_cli.clone(),
        });
    }

    Ok(builds)
}
