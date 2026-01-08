use std::{collections::HashMap, sync::Arc};

use tokio::sync::{mpsc, oneshot};

use crate::{
    actors::{Addr, Response},
    display::{Progress, ProgressBar, UpdateKind},
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
        tx: oneshot::Sender<ProgressAddr>,
    },

    Println {
        msg: Arc<str>,
    },

    RegisterGrammar {
        git_ref: GitRef,
        language: Arc<str>,
        name: Arc<str>,
        num_tasks: usize,
        tx: oneshot::Sender<ProgressAddr>,
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

    pub async fn add_grammar<S: Into<Arc<str>>>(
        &self,
        git_ref: GitRef,
        language: S,
        name: S,
        num_tasks: usize,
    ) -> ProgressAddr {
        self.request(|tx| DisplayMessage::RegisterGrammar {
            git_ref,
            language: language.into(),
            name: name.into(),
            num_tasks,
            tx,
        })
        .await
    }

    pub async fn add_language<S: Into<Arc<str>>>(
        &self,
        git_ref: GitRef,
        name: S,
        num_tasks: usize,
    ) -> ProgressAddr {
        self.request(|tx| DisplayMessage::RegisterLanguage {
            git_ref,
            name: name.into(),
            num_tasks,
            tx,
        })
        .await
    }

    pub async fn println<S: Into<Arc<str>>>(&self, msg: S) {
        self.fire(DisplayMessage::Println { msg: msg.into() }).await;
    }

    pub async fn remove_language<S: Into<Arc<str>>>(&self, name: S) -> TsdlResult<()> {
        self.fire(DisplayMessage::UnregisterLanguage { name: name.into() })
            .await;
        Ok(())
    }

    pub async fn tick(&self) {
        self.fire(DisplayMessage::Tick {}).await;
    }
}

/// The Task Handle: Dedicated to controlling a specific progress bar.
#[derive(Debug, Clone)]
pub struct ProgressAddr {
    id: u64,
    tx: mpsc::Sender<DisplayMessage>,
}

impl ProgressAddr {
    /// Takes Into<String> directly as the message must be owned to be sent
    pub fn msg<S: Into<String>>(&self, msg: S) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Msg,
            msg: msg.into(),
        });
    }

    pub fn step<S: Into<String>>(&self, msg: S) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Step,
            msg: msg.into(),
        });
    }

    pub fn fin<S: Into<String>>(&self, msg: S) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Fin,
            msg: msg.into(),
        });
    }

    pub fn err<S: Into<String>>(&self, msg: S) {
        let _ = self.tx.try_send(DisplayMessage::Update {
            id: self.id,
            kind: UpdateKind::Err,
            msg: msg.into(),
        });
    }
}

pub struct DisplayActor {
    handles: HashMap<u64, ProgressBar>,
    next_id: u64,
    progress: Progress,
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

                DisplayMessage::Println { msg } => {
                    self.progress.prinltn(msg);
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
                    self.progress.tick();
                }
            }
        }
    }

    fn register<F>(&mut self, create: F) -> ProgressAddr
    where
        F: FnOnce(&mut Progress) -> ProgressBar,
    {
        // 1. Create inner handle
        let inner = create(&mut self.progress);

        // 2. Register in actor state
        let id = self.next_id;
        self.next_id += 1;
        self.handles.insert(id, inner);

        // 3. Return client handle
        ProgressAddr {
            id,
            tx: self.tx.clone(),
        }
    }

    #[must_use]
    pub fn spawn(progress: Progress) -> DisplayAddr {
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
