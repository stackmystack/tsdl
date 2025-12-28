use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::{mpsc, oneshot};

use crate::{
    actors::{Addr, Response},
    display::{Progress, ProgressBar, UpdateKind},
    error::TsdlError,
    git::GitRef,
    TsdlResult,
};

#[derive(Debug)]
#[allow(dead_code)]
enum DisplayResponseKind<'a> {
    RegisterGrammar { language: &'a str, name: &'a str },
    RegisterLanguage { name: &'a str },
}

#[derive(Debug)]
pub enum DisplayMessage {
    RegisterLanguage {
        git_ref: GitRef,
        name: Arc<str>,
        num_tasks: usize,
        tx: oneshot::Sender<TsdlResult<ProgressAddr>>,
    },

    RegisterGrammar {
        git_ref: GitRef,
        language: Arc<str>,
        name: Arc<str>,
        num_tasks: usize,
        tx: oneshot::Sender<TsdlResult<ProgressAddr>>,
    },

    UnregisterLanguage {
        name: Arc<str>,
    },

    Update {
        id: u64,
        kind: UpdateKind,
        msg: String,
    },
    Tick,
}

/// The Manager Handle: Only used to register/unregister tasks.
#[derive(Debug, Clone)]
pub struct DisplayAddr {
    tx: mpsc::Sender<DisplayMessage>,
}

impl Addr for DisplayAddr {
    type Message = DisplayMessage;

    fn name() -> &'static str {
        "DisplayAddr"
    }

    fn sender(&self) -> &mpsc::Sender<Self::Message> {
        &self.tx
    }
}

impl DisplayAddr {
    #[must_use]
    pub fn new(tx: mpsc::Sender<DisplayMessage>) -> Self {
        Self { tx }
    }

    pub async fn add_grammar(
        &self,
        git_ref: GitRef,
        language: Arc<str>,
        name: Arc<str>,
        num_tasks: usize,
    ) -> TsdlResult<ProgressAddr> {
        self.request(|tx| DisplayMessage::RegisterGrammar {
            git_ref,
            language,
            name,
            num_tasks,
            tx,
        })
        .await
    }

    pub async fn add_language(
        &self,
        git_ref: GitRef,
        name: Arc<str>,
        num_tasks: usize,
    ) -> TsdlResult<ProgressAddr> {
        self.request(|tx| DisplayMessage::RegisterLanguage {
            git_ref,
            name,
            num_tasks,
            tx,
        })
        .await
    }

    pub async fn remove_language(&self, name: Arc<str>) -> TsdlResult<()> {
        self.fire(DisplayMessage::UnregisterLanguage { name }).await;
        Ok(())
    }
}

/// The Task Handle: Dedicated to controlling a specific progress bar.
#[derive(Debug, Clone)]
pub struct ProgressAddr {
    id: u64,
    tx: mpsc::Sender<DisplayMessage>,
}

impl ProgressAddr {
    pub fn msg<'a, S>(&self, msg: S)
    where
        S: Into<Cow<'a, str>>,
    {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Msg,
            msg: msg.into().into_owned(),
        });
    }

    pub fn step<'a, S>(&self, msg: S)
    where
        S: Into<Cow<'a, str>>,
    {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Step,
            msg: msg.into().into_owned(),
        });
    }

    pub fn fin<'a, S>(&self, msg: S)
    where
        S: Into<Cow<'a, str>>,
    {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Fin,
            msg: msg.into().into_owned(),
        });
    }

    pub fn err<'a, S>(&self, msg: S)
    where
        S: Into<Cow<'a, str>>,
    {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Err,
            msg: msg.into().into_owned(),
        });
    }
}

pub struct DisplayActor {
    handles: HashMap<u64, ProgressBar>,
    next_id: u64,
    progress: Arc<Mutex<Progress>>,
    rx: mpsc::Receiver<DisplayMessage>,
    tx: mpsc::Sender<DisplayMessage>,
}

impl DisplayActor {
    fn finish<F>(&mut self, id: u64, f: F)
    where
        F: FnOnce(&ProgressBar),
    {
        self.forward(id, f);
        self.handles.remove(&id);
    }

    fn forward<F>(&self, id: u64, f: F)
    where
        F: FnOnce(&ProgressBar),
    {
        if let Some(h) = self.handles.get(&id) {
            f(h);
        }
    }

    async fn run(mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                DisplayMessage::RegisterLanguage {
                    git_ref,
                    ref name,
                    num_tasks,
                    tx,
                } => {
                    let res = self.register({
                        let name = name.clone();
                        move |p| p.register(name, git_ref, num_tasks)
                    });

                    Response {
                        tx,
                        kind: DisplayResponseKind::RegisterLanguage { name },
                    }
                    .send(res);
                }

                DisplayMessage::RegisterGrammar {
                    git_ref,
                    ref language,
                    ref name,
                    num_tasks,
                    tx,
                } => {
                    let res = self.register(|p| {
                        let language = language.clone();
                        let name = name.clone();
                        p.register(format!("{language}/{name}").into(), git_ref, num_tasks)
                    });
                    Response {
                        tx,
                        kind: DisplayResponseKind::RegisterGrammar { language, name },
                    }
                    .send(res);
                }

                DisplayMessage::UnregisterLanguage { name } => {
                    self.handles.retain(|_, h| name != h.name);
                }

                DisplayMessage::Update { id, kind, ref msg } => match kind {
                    UpdateKind::Msg => self.forward(id, |h| h.msg(msg)),
                    UpdateKind::Step => self.forward(id, |h| h.step(msg)),
                    UpdateKind::Fin => self.finish(id, |h| h.fin(msg)),
                    UpdateKind::Err => self.finish(id, |h| h.err(msg)),
                },

                DisplayMessage::Tick => {
                    if let Ok(p) = self.progress.lock() {
                        p.tick();
                    }
                }
            }
        }
    }

    /// TODO: I'd really like to remove the Mutex.
    fn register<F>(&mut self, create: F) -> TsdlResult<ProgressAddr>
    where
        F: FnOnce(&mut Progress) -> ProgressBar,
    {
        // 1. Create inner handle
        let inner = {
            let mut progress = self
                .progress
                .lock()
                .map_err(|_| TsdlError::message("Lock poisoned"))?;
            create(&mut progress)
        };

        // 2. Register in actor state
        let id = self.next_id;
        self.next_id += 1;
        self.handles.insert(id, inner);

        // 3. Return client handle
        Ok(ProgressAddr {
            id,
            tx: self.tx.clone(),
        })
    }

    pub fn spawn(progress: Arc<Mutex<Progress>>) -> DisplayAddr {
        let (tx, rx) = mpsc::channel(64);
        let actor = Self {
            handles: HashMap::new(),
            next_id: 1,
            progress,
            rx,
            tx: tx.clone(),
        };
        tokio::spawn(actor.run());
        DisplayAddr::new(tx)
    }
}
