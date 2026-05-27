use std::{io, path::PathBuf, sync::Arc};

use tokio::{
    fs,
    sync::{mpsc, oneshot},
};
use tracing::info;

use crate::{
    actors::{Addr, Response},
    build, cache, parser, Result,
};

#[derive(Debug)]
#[allow(dead_code)]
enum ResponseKind<'a> {
    CacheGet {
        name: &'a cache::Key,
    },
    HasCompatibleEntries {
        language: &'a parser::LanguageName,
    },
    NeedsRebuild {
        name: &'a cache::Key,
        hash: &'a cache::GrammarHash,
    },
    SaveComplete,
}

#[derive(Debug)]
pub enum CacheMessage {
    /// Query if a parser needs rebuild
    NeedsRebuild {
        hash: cache::GrammarHash,
        name: cache::Key,
        source: cache::Source,
        spec: Arc<build::Spec>,
        artifacts: Vec<PathBuf>,
        tx: oneshot::Sender<cache::Decision>,
    },
    /// `cache::Update` a cache entry
    Update {
        entry: cache::Entry,
        name: cache::Key,
    },
    /// Save cache to disk
    Save { tx: oneshot::Sender<Result<()>> },
    /// Check if cache contains entries compatible with a language spec.
    HasCompatibleEntries {
        language: parser::LanguageName,
        spec: Arc<build::Spec>,
        tx: oneshot::Sender<bool>,
    },
    /// Get a cache entry
    Get {
        name: cache::Key,
        tx: oneshot::Sender<Option<cache::Entry>>,
    },
}

/// The Cache Handle: Public interface for sending cache operations
#[derive(Debug, Clone)]
pub struct CacheAddr {
    tx: mpsc::Sender<CacheMessage>,
}

impl Addr for CacheAddr {
    type Message = CacheMessage;

    fn name() -> &'static str {
        "CacheAddr"
    }

    fn sender(&self) -> &mpsc::Sender<Self::Message> {
        &self.tx
    }
}

impl CacheAddr {
    #[must_use]
    pub fn new(tx: mpsc::Sender<CacheMessage>) -> Self {
        Self { tx }
    }

    /// Accepts any string type (String, &str, Arc<str>) with minimal cloning
    pub async fn get(&self, name: cache::Key) -> Option<cache::Entry> {
        self.request(|tx| CacheMessage::Get { name, tx }).await
    }

    pub async fn has_compatible_entries(
        &self,
        language: parser::LanguageName,
        spec: Arc<build::Spec>,
    ) -> bool {
        self.request(|tx| CacheMessage::HasCompatibleEntries { language, spec, tx })
            .await
    }

    pub async fn needs_rebuild(
        &self,
        name: cache::Key,
        hash: cache::GrammarHash,
        spec: Arc<build::Spec>,
        source: cache::Source,
        artifacts: Vec<PathBuf>,
    ) -> cache::Decision {
        self.request(|tx| CacheMessage::NeedsRebuild {
            name,
            hash,
            source,
            spec,
            artifacts,
            tx,
        })
        .await
    }

    pub async fn save(&self) -> Result<()> {
        self.request(|tx| CacheMessage::Save { tx }).await
    }

    pub async fn update(&self, update: cache::Update) {
        self.fire(CacheMessage::Update {
            entry: update.entry,
            name: update.name,
        })
        .await;
    }
}

/// The Cache Actor: Manages cache state and processes messages
pub struct CacheActor {
    db: cache::Db,
    force: bool,
    rx: mpsc::Receiver<CacheMessage>,
}

async fn verify_artifacts(artifacts: Vec<PathBuf>) -> cache::Decision {
    let mut reasons = Vec::new();

    for path in artifacts {
        match fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => reasons.push(cache::MissReason::ArtifactNotFile { path }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                reasons.push(cache::MissReason::ArtifactMissing { path });
            }
            Err(err) => reasons.push(cache::MissReason::ArtifactInaccessible {
                path,
                error: err.to_string(),
            }),
        }
    }

    cache::Decision::from_reasons(reasons)
}

impl CacheActor {
    async fn run(mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                CacheMessage::NeedsRebuild {
                    hash,
                    name,
                    spec,
                    source,
                    artifacts,
                    tx,
                } => {
                    let decision = if self.force {
                        cache::Decision::miss(cache::MissReason::CacheIgnored)
                    } else {
                        let decision = self.db.rebuild_decision(&name, &hash, &spec, &source);
                        if decision.is_hit() {
                            verify_artifacts(artifacts).await
                        } else {
                            decision
                        }
                    };
                    if decision.needs_rebuild() {
                        info!("Cache miss for {name}: {}", decision.short_message());
                    }

                    Response {
                        tx,
                        kind: ResponseKind::NeedsRebuild {
                            name: &name,
                            hash: &hash,
                        },
                    }
                    .send(decision);
                }

                CacheMessage::Update { entry, name } => {
                    self.db.set(name, entry);
                }

                CacheMessage::Save { tx } => {
                    Response {
                        tx,
                        kind: ResponseKind::SaveComplete,
                    }
                    .send(self.db.save().await);
                }

                CacheMessage::HasCompatibleEntries { language, spec, tx } => {
                    Response {
                        tx,
                        kind: ResponseKind::HasCompatibleEntries {
                            language: &language,
                        },
                    }
                    .send(
                        !self.force
                            && self
                                .db
                                .parsers
                                .iter()
                                .find(|(key, _)| {
                                    key.as_str()
                                        .starts_with(&cache::Key::language_prefix(&language))
                                })
                                .is_some_and(|(_, entry)| entry.spec == spec),
                    );
                }

                CacheMessage::Get { name, tx } => {
                    Response {
                        tx,
                        kind: ResponseKind::CacheGet { name: &name },
                    }
                    .send(self.db.get(&name).cloned());
                }
            }
        }
    }

    #[must_use]
    pub fn spawn(db: cache::Db, force: bool) -> CacheAddr {
        let (tx, rx) = mpsc::channel(64);
        let actor = Self { db, force, rx };
        tokio::spawn(actor.run());
        CacheAddr::new(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn verify_artifacts_hits_when_all_paths_are_files() {
        let temp = TempDir::new().unwrap();
        let artifact = temp.path().join("parser.so");
        fs::write(&artifact, b"parser").await.unwrap();

        assert_eq!(verify_artifacts(vec![artifact]).await, cache::Decision::Hit);
    }

    #[tokio::test]
    async fn verify_artifacts_reports_missing_and_non_file_paths() {
        let temp = TempDir::new().unwrap();
        let missing = temp.path().join("missing.so");
        let directory = temp.path().join("parser.so");
        fs::create_dir(&directory).await.unwrap();

        assert_eq!(
            verify_artifacts(vec![missing.clone(), directory.clone()]).await,
            cache::Decision::Miss(crate::cache::Miss {
                reasons: vec![
                    cache::MissReason::ArtifactMissing { path: missing },
                    cache::MissReason::ArtifactNotFile { path: directory },
                ],
            })
        );
    }
}
