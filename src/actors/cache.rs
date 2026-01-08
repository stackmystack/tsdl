use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::{
    actors::{Addr, Response},
    build::BuildSpec,
    cache::{Db, Entry, Update},
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
        spec: Arc<BuildSpec>,
        tx: oneshot::Sender<bool>,
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
    ) -> bool {
        self.request(|tx| CacheMessage::NeedsRebuild {
            name: name.into(),
            hash: hash.into(),
            spec,
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

impl CacheActor {
    async fn run(mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                CacheMessage::NeedsRebuild {
                    hash,
                    name,
                    spec,
                    tx,
                } => {
                    Response {
                        tx,
                        kind: ResponseKind::NeedsRebuild {
                            name: &name,
                            hash: &hash,
                        },
                    }
                    .send(self.db.needs_rebuild(&name, &hash, &spec));
                }

                CacheMessage::Update { entry, name } => {
                    self.db.set(name.to_string(), entry);
                }

                CacheMessage::Save { tx } => {
                    Response {
                        tx,
                        kind: ResponseKind::SaveComplete,
                    }
                    .send(self.db.save());
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
