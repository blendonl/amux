use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{anyhow, bail, Result};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Notify, Semaphore};
use tracing::{debug, warn};

use crate::protocol::{ChannelId, ClientMessage, Duplex, PeerMessage, ServerMessage};

pub const CREDIT_WINDOW: u32 = 4;
const QUEUE_CAPACITY: usize = 64;
const QUEUE_BYTES: usize = 16 * 1024 * 1024;
const HOST_LANE_CAPACITY: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEnd {
    ClosedByHost,
    HostStopped,
    LinkDown,
    Overflow,
}

impl fmt::Display for ChannelEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ClosedByHost => "the host closed the channel",
            Self::HostStopped => "the host stopped",
            Self::LinkDown => "the link went down",
            Self::Overflow => "the host sent more than it was allowed to",
        })
    }
}

pub(super) struct Channels {
    bulk: mpsc::Sender<PeerMessage>,
    control: mpsc::Sender<PeerMessage>,
    stop: Arc<Notify>,
    table: Mutex<Table>,
}

#[derive(Default)]
struct Table {
    last_opened: u64,
    opened: HashMap<ChannelId, Opened>,
    hosted: HashMap<ChannelId, Hosted>,
    down: bool,
}

struct Opened {
    inbound: QueueSender<ServerMessage>,
    end: Arc<EndCell>,
}

struct Hosted {
    inbound: QueueSender<ClientMessage>,
    credit: Arc<Semaphore>,
}

impl Channels {
    pub fn new(
        bulk: mpsc::Sender<PeerMessage>,
        control: mpsc::Sender<PeerMessage>,
        stop: Arc<Notify>,
    ) -> Arc<Self> {
        Arc::new(Self {
            bulk,
            control,
            stop,
            table: Mutex::default(),
        })
    }

    pub async fn open(self: &Arc<Self>, first: ClientMessage) -> Result<Channel> {
        let (inbound, queue) = queue();
        let end = Arc::new(EndCell::default());
        let id = {
            let mut table = self.table();
            if table.down {
                bail!("the link is down");
            }
            table.last_opened += 1;
            let id = ChannelId(table.last_opened);
            table.opened.insert(
                id,
                Opened {
                    inbound,
                    end: Arc::clone(&end),
                },
            );
            id
        };
        let channel = Channel {
            id,
            queue,
            end,
            channels: Arc::clone(self),
        };
        self.bulk
            .send(PeerMessage::ChannelOpen { id, first })
            .await
            .map_err(|_| anyhow!("the link is down"))?;
        debug!(%id, "channel opened");
        Ok(channel)
    }

    pub fn accept(
        self: &Arc<Self>,
        id: ChannelId,
        first: ClientMessage,
        len: usize,
    ) -> Option<Duplex<ClientMessage, ServerMessage>> {
        let (inbound, queue) = queue();
        if inbound.try_push(first, len).is_err() {
            self.try_control(PeerMessage::ChannelClose {
                id,
                from_opener: false,
            });
            return None;
        }
        let credit = Arc::new(Semaphore::new(CREDIT_WINDOW as usize));
        {
            let mut table = self.table();
            if table.down || table.hosted.contains_key(&id) {
                warn!(%id, "ignoring a channel that is already open");
                return None;
            }
            table.hosted.insert(
                id,
                Hosted {
                    inbound,
                    credit: Arc::clone(&credit),
                },
            );
        }
        let (incoming_sender, incoming) = mpsc::channel(HOST_LANE_CAPACITY);
        let (outgoing, outgoing_receiver) = mpsc::channel(HOST_LANE_CAPACITY);
        tokio::spawn(feed(queue, incoming_sender));
        tokio::spawn(Arc::clone(self).pump(id, outgoing_receiver, credit));
        debug!(%id, "hosting a channel");
        Some(Duplex { incoming, outgoing })
    }

    pub fn to_host(&self, id: ChannelId, message: ClientMessage, len: usize) -> bool {
        let mut table = self.table();
        let Some(hosted) = table.hosted.get(&id) else {
            return true;
        };
        match hosted.inbound.try_push(message, len) {
            Ok(()) | Err(PushError::Closed) => true,
            Err(PushError::Full) => {
                warn!(%id, "closing a channel whose session fell behind its input");
                if let Some(hosted) = table.hosted.remove(&id) {
                    hosted.credit.close();
                }
                drop(table);
                self.try_control(PeerMessage::ChannelClose {
                    id,
                    from_opener: false,
                })
            }
        }
    }

    pub fn to_client(&self, id: ChannelId, message: ServerMessage, len: usize) -> bool {
        let mut table = self.table();
        let Some(opened) = table.opened.get(&id) else {
            let ours = id.0 <= table.last_opened;
            drop(table);
            return !ours
                || self.try_control(PeerMessage::ChannelClose {
                    id,
                    from_opener: true,
                });
        };
        match opened.inbound.try_push(message, len) {
            Ok(()) | Err(PushError::Closed) => true,
            Err(PushError::Full) => {
                warn!(%id, "closing a channel whose host ignored its credit");
                if let Some(opened) = table.opened.remove(&id) {
                    opened.end.set(ChannelEnd::Overflow);
                }
                drop(table);
                self.try_control(PeerMessage::ChannelClose {
                    id,
                    from_opener: true,
                })
            }
        }
    }

    pub fn credit(&self, id: ChannelId, credit: u32) {
        if let Some(hosted) = self.table().hosted.get(&id) {
            let window = CREDIT_WINDOW as usize;
            let room = window.saturating_sub(hosted.credit.available_permits());
            hosted.credit.add_permits((credit as usize).min(room));
        }
    }

    pub fn closed(&self, id: ChannelId, from_opener: bool) {
        let mut table = self.table();
        if from_opener {
            if let Some(hosted) = table.hosted.remove(&id) {
                hosted.credit.close();
                debug!(%id, "the opener closed a hosted channel");
            }
        } else if let Some(opened) = table.opened.remove(&id) {
            opened.end.set(ChannelEnd::ClosedByHost);
            debug!(%id, "the host closed a channel");
        }
    }

    pub fn host_stopped(&self) {
        let mut table = self.table();
        for (_, opened) in table.opened.drain() {
            opened.end.set(ChannelEnd::HostStopped);
        }
    }

    pub fn link_down(&self) {
        let mut table = self.table();
        table.down = true;
        for (_, opened) in table.opened.drain() {
            opened.end.set(ChannelEnd::LinkDown);
        }
        for (_, hosted) in table.hosted.drain() {
            hosted.credit.close();
        }
    }

    pub fn open_count(&self) -> (usize, usize) {
        let table = self.table();
        (table.opened.len(), table.hosted.len())
    }

    async fn pump(
        self: Arc<Self>,
        id: ChannelId,
        mut outgoing: mpsc::Receiver<ServerMessage>,
        credit: Arc<Semaphore>,
    ) {
        loop {
            let Ok(permit) = credit.acquire().await else {
                return;
            };
            permit.forget();
            let Some(message) = outgoing.recv().await else {
                break;
            };
            let message = PeerMessage::ChannelToClient { id, message };
            if self.bulk.send(message).await.is_err() {
                return;
            }
        }
        let removed = self.table().hosted.remove(&id).is_some();
        if removed {
            let close = PeerMessage::ChannelClose {
                id,
                from_opener: false,
            };
            let _ = self.bulk.send(close).await;
            debug!(%id, "a hosted channel ended");
        }
    }

    fn try_control(&self, message: PeerMessage) -> bool {
        !matches!(self.control.try_send(message), Err(TrySendError::Full(_)))
    }

    fn control(&self, message: PeerMessage) {
        if !self.try_control(message) {
            warn!("dropping the link, the peer stopped reading");
            self.stop.notify_one();
        }
    }

    fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct Channel {
    id: ChannelId,
    queue: QueueReceiver<ServerMessage>,
    end: Arc<EndCell>,
    channels: Arc<Channels>,
}

impl Channel {
    pub fn id(&self) -> ChannelId {
        self.id
    }

    pub async fn recv(&mut self) -> Option<ServerMessage> {
        self.queue.recv().await
    }

    pub fn end(&self) -> ChannelEnd {
        self.end.get().unwrap_or(ChannelEnd::LinkDown)
    }

    pub async fn send(&self, message: ClientMessage) -> Result<()> {
        let message = PeerMessage::ChannelToHost {
            id: self.id,
            message,
        };
        self.channels
            .bulk
            .send(message)
            .await
            .map_err(|_| anyhow!("the link is down"))
    }

    pub fn delivered(&self) {
        self.channels.control(PeerMessage::ChannelCredit {
            id: self.id,
            credit: 1,
        });
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        let removed = self.channels.table().opened.remove(&self.id).is_some();
        if removed {
            self.channels.control(PeerMessage::ChannelClose {
                id: self.id,
                from_opener: true,
            });
            debug!(id = %self.id, "channel closed");
        }
    }
}

#[derive(Default)]
struct EndCell(Mutex<Option<ChannelEnd>>);

impl EndCell {
    fn set(&self, end: ChannelEnd) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(end);
    }

    fn get(&self) -> Option<ChannelEnd> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

enum PushError {
    Full,
    Closed,
}

struct QueueSender<T> {
    sender: mpsc::Sender<(T, usize)>,
    bytes: Arc<AtomicUsize>,
}

struct QueueReceiver<T> {
    receiver: mpsc::Receiver<(T, usize)>,
    bytes: Arc<AtomicUsize>,
}

fn queue<T>() -> (QueueSender<T>, QueueReceiver<T>) {
    let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
    let bytes = Arc::new(AtomicUsize::new(0));
    (
        QueueSender {
            sender,
            bytes: Arc::clone(&bytes),
        },
        QueueReceiver { receiver, bytes },
    )
}

impl<T> QueueSender<T> {
    fn try_push(&self, item: T, len: usize) -> Result<(), PushError> {
        let queued = self.bytes.fetch_add(len, Ordering::AcqRel) + len;
        if queued > QUEUE_BYTES {
            self.bytes.fetch_sub(len, Ordering::AcqRel);
            return Err(PushError::Full);
        }
        self.sender.try_send((item, len)).map_err(|err| {
            self.bytes.fetch_sub(len, Ordering::AcqRel);
            match err {
                TrySendError::Full(_) => PushError::Full,
                TrySendError::Closed(_) => PushError::Closed,
            }
        })
    }
}

impl<T> QueueReceiver<T> {
    async fn recv(&mut self) -> Option<T> {
        let (item, len) = self.receiver.recv().await?;
        self.bytes.fetch_sub(len, Ordering::AcqRel);
        Some(item)
    }
}

async fn feed<T>(mut queue: QueueReceiver<T>, sender: mpsc::Sender<T>) {
    loop {
        let item = tokio::select! {
            item = queue.recv() => item,
            () = sender.closed() => return,
        };
        let Some(item) = item else {
            return;
        };
        if sender.send(item).await.is_err() {
            return;
        }
    }
}
