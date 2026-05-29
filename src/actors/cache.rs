//! Cache actor: async message handler for cache reads, rebuild decisions, and
//! updates.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tracing::info;

use crate::{
    actors::{Addr, Response},
    build, cache, parser, Result,
};

// ============================================================
// Enums
// ============================================================

/// Messages sent to the cache actor.
///
/// Each variant is a cache operation: query, update, save, or
/// compatibility check. Response-bearing variants carry a
/// [`oneshot::Sender`] for the reply.
#[derive(Debug)]
pub enum CacheMessage {
    /// Query if a parser needs rebuild
    NeedsRebuild {
        hash: cache::GrammarHash,
        name: cache::Key,
        revision: cache::Revision,
        spec: Arc<build::Spec>,
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

// ============================================================
// Structs
// ============================================================

/// The Cache Actor: Manages cache state and processes messages
pub struct CacheActor {
    /// In-memory cache database.
    db: cache::Db,
    /// File-backed storage for persistence.
    store: cache::Store,
    /// When true, every query returns a miss.
    force: bool,
    /// Message receiver channel.
    rx: mpsc::Receiver<CacheMessage>,
}

/// The Cache Handle: Public interface for sending cache operations
#[derive(Debug, Clone)]
pub struct CacheAddr {
    /// Sender half of the channel to the cache actor.
    tx: mpsc::Sender<CacheMessage>,
}

// ============================================================
// Impls
// ============================================================

impl Addr for CacheAddr {
    type Message = CacheMessage;

    fn name() -> &'static str {
        "CacheAddr"
    }

    fn sender(&self) -> &mpsc::Sender<Self::Message> {
        &self.tx
    }
}

impl CacheActor {
    /// Process incoming cache messages until the channel closes.
    async fn run(mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                CacheMessage::NeedsRebuild {
                    hash,
                    name,
                    spec,
                    revision,
                    tx,
                } => {
                    let decision = if self.force {
                        cache::Decision::miss(cache::MissReason::CacheIgnored)
                    } else {
                        self.db.rebuild_decision(&name, &hash, &revision, &spec)
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
                    .send(self.store.save(&self.db).await);
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
                                .has_compatible_entry_for_language(&language, spec.as_ref()),
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

    /// Spawn the cache actor in a background task and return an address handle.
    #[must_use]
    pub fn spawn(db: cache::Db, force: bool, store: cache::Store) -> CacheAddr {
        let (tx, rx) = mpsc::channel(64);
        let actor = Self {
            db,
            store,
            force,
            rx,
        };
        tokio::spawn(actor.run());
        CacheAddr::new(tx)
    }
}

impl CacheAddr {
    /// Create a new cache address from a sender.
    #[must_use]
    pub fn new(tx: mpsc::Sender<CacheMessage>) -> Self {
        Self { tx }
    }

    /// Accepts any string type (`String`, `&str`, `Arc<str>`) with minimal cloning
    pub async fn get(&self, name: cache::Key) -> Option<cache::Entry> {
        self.request(|tx| CacheMessage::Get { name, tx }).await
    }

    /// Check if the cache has compatible entries for a language (same repo + git ref).
    pub async fn has_compatible_entries(
        &self,
        language: parser::LanguageName,
        spec: Arc<build::Spec>,
    ) -> bool {
        self.request(|tx| CacheMessage::HasCompatibleEntries { language, spec, tx })
            .await
    }

    /// Ask the cache actor whether a parser needs rebuilding.
    pub async fn needs_rebuild(
        &self,
        name: cache::Key,
        hash: cache::GrammarHash,
        revision: cache::Revision,
        spec: Arc<build::Spec>,
    ) -> cache::Decision {
        self.request(|tx| CacheMessage::NeedsRebuild {
            name,
            hash,
            revision,
            spec,
            tx,
        })
        .await
    }

    /// Save the cache to disk.
    pub async fn save(&self) -> Result<()> {
        self.request(|tx| CacheMessage::Save { tx }).await
    }

    /// Send a cache update (fire-and-forget).
    pub async fn update(&self, update: cache::Update) {
        self.fire(CacheMessage::Update {
            entry: update.entry,
            name: update.name,
        })
        .await;
    }
}
