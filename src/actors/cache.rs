use std::{io, path::PathBuf, sync::Arc};

use tokio::{
    fs,
    sync::{mpsc, oneshot},
};
use tracing::info;

use crate::{
    actors::{Addr, Response},
    build::BuildSpec,
    cache::{CacheDecision, CacheMissReason, Db, Entry, Source, Update},
    TsdlResult,
};

#[derive(Debug)]
#[allow(dead_code)]
enum ResponseKind<'a> {
    CacheGet { name: &'a str },
    NeedsClone { language: &'a str },
    NeedsRebuild { name: &'a str, hash: &'a str },
    SaveComplete,
}

#[derive(Debug)]
pub enum CacheMessage {
    /// Query if a parser needs rebuild
    NeedsRebuild {
        hash: Arc<str>,
        name: Arc<str>,
        source: Source,
        spec: Arc<BuildSpec>,
        artifacts: Vec<PathBuf>,
        tx: oneshot::Sender<CacheDecision>,
    },
    /// Update a cache entry
    Update { entry: Entry, name: Arc<str> },
    /// Save cache to disk
    Save { tx: oneshot::Sender<TsdlResult<()>> },
    /// Check if clone is needed for a language
    NeedsClone {
        language: Arc<str>,
        spec: Arc<BuildSpec>,
        tx: oneshot::Sender<bool>,
    },
    /// Get a cache entry
    Get {
        name: Arc<str>,
        tx: oneshot::Sender<Option<Entry>>,
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
    pub async fn get<S: Into<Arc<str>>>(&self, name: S) -> Option<Entry> {
        self.request(|tx| CacheMessage::Get {
            name: name.into(),
            tx,
        })
        .await
    }

    pub async fn needs_clone<S: Into<Arc<str>>>(&self, language: S, spec: Arc<BuildSpec>) -> bool {
        self.request(|tx| CacheMessage::NeedsClone {
            language: language.into(),
            spec,
            tx,
        })
        .await
    }

    pub async fn needs_rebuild<S: Into<Arc<str>>>(
        &self,
        name: S,
        hash: S,
        spec: Arc<BuildSpec>,
        source: Source,
        artifacts: Vec<PathBuf>,
    ) -> CacheDecision {
        self.request(|tx| CacheMessage::NeedsRebuild {
            name: name.into(),
            hash: hash.into(),
            source,
            spec,
            artifacts,
            tx,
        })
        .await
    }

    pub async fn save(&self) -> TsdlResult<()> {
        self.request(|tx| CacheMessage::Save { tx }).await
    }

    pub async fn update(&self, update: Update) {
        self.fire(CacheMessage::Update {
            entry: update.entry,
            name: update.name,
        })
        .await;
    }
}

/// The Cache Actor: Manages cache state and processes messages
pub struct CacheActor {
    db: Db,
    force: bool,
    rx: mpsc::Receiver<CacheMessage>,
}

async fn verify_artifacts(artifacts: Vec<PathBuf>) -> CacheDecision {
    let mut reasons = Vec::new();

    for path in artifacts {
        match fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => reasons.push(CacheMissReason::ArtifactNotFile { path }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                reasons.push(CacheMissReason::ArtifactMissing { path });
            }
            Err(err) => reasons.push(CacheMissReason::ArtifactInaccessible {
                path,
                error: err.to_string(),
            }),
        }
    }

    CacheDecision::from_reasons(reasons)
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
                        CacheDecision::miss(CacheMissReason::CacheIgnored)
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
                    self.db.set(name.to_string(), entry);
                }

                CacheMessage::Save { tx } => {
                    Response {
                        tx,
                        kind: ResponseKind::SaveComplete,
                    }
                    .send(self.db.save().await);
                }

                CacheMessage::NeedsClone { language, spec, tx } => {
                    Response {
                        tx,
                        kind: ResponseKind::NeedsClone {
                            language: &language,
                        },
                    }
                    .send(
                        self.force
                            || self
                                .db
                                .parsers
                                .iter()
                                .find(|(key, _)| key.starts_with(&format!("{language}/")))
                                .is_none_or(|(_, entry)| entry.spec != spec),
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
    pub fn spawn(db: Db, force: bool) -> CacheAddr {
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

        assert_eq!(verify_artifacts(vec![artifact]).await, CacheDecision::Hit);
    }

    #[tokio::test]
    async fn verify_artifacts_reports_missing_and_non_file_paths() {
        let temp = TempDir::new().unwrap();
        let missing = temp.path().join("missing.so");
        let directory = temp.path().join("parser.so");
        fs::create_dir(&directory).await.unwrap();

        assert_eq!(
            verify_artifacts(vec![missing.clone(), directory.clone()]).await,
            CacheDecision::Miss(crate::cache::CacheMiss {
                reasons: vec![
                    CacheMissReason::ArtifactMissing { path: missing },
                    CacheMissReason::ArtifactNotFile { path: directory },
                ],
            })
        );
    }
}
