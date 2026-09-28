use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch, OwnedSemaphorePermit};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::noise::{self, Secured};
use super::{Cluster, NoiseHandshake, FLUSH_GRACE};
use crate::protocol::TcpKind;

pub const LAN_PORT_FILE: &str = "lan-port";
pub(super) const MAX_PENDING_HANDSHAKES: usize = 16;
const PRE_AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

pub struct Listener {
    local: SocketAddr,
    task: JoinHandle<()>,
}

impl Listener {
    pub async fn bind(cluster: &Arc<Cluster>, address: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("binding {address}"))?;
        let local = listener
            .local_addr()
            .with_context(|| format!("reading the address bound for {address}"))?;
        let task = tokio::spawn(listen(Arc::clone(cluster), listener));
        Ok(Self { local, task })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn listen(cluster: Arc<Cluster>, listener: TcpListener) {
    loop {
        let (stream, remote) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                warn!("accepting a tcp connection failed: {err}");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        let Ok(permit) = Arc::clone(&cluster.handshakes).try_acquire_owned() else {
            debug!(%remote, "dropping a tcp connection, too many handshakes are running");
            continue;
        };
        tokio::spawn(serve(Arc::clone(&cluster), stream, remote, permit));
    }
}

async fn serve(
    cluster: Arc<Cluster>,
    stream: TcpStream,
    remote: SocketAddr,
    permit: OwnedSemaphorePermit,
) {
    let _ = stream.set_nodelay(true);
    let handshake =
        tokio::time::timeout(PRE_AUTH_TIMEOUT, handshake(&cluster, stream, remote)).await;
    drop(permit);
    match handshake {
        Ok(Ok(Some((handshake, flushed)))) => {
            let _transport = cluster.count_transport();
            cluster.conclude(handshake, None).await;
            let _ = tokio::time::timeout(FLUSH_GRACE, flushed).await;
        }
        Ok(Ok(None)) => {}
        Ok(Err(err)) => info!(%remote, "a tcp connection failed: {err:#}"),
        Err(_) => info!(%remote, "a tcp connection did not finish its handshake in time"),
    }
}

async fn handshake(
    cluster: &Arc<Cluster>,
    mut stream: TcpStream,
    remote: SocketAddr,
) -> Result<Option<(NoiseHandshake, oneshot::Receiver<()>)>> {
    let Some(kind) = noise::read_opening(&mut stream).await? else {
        return Ok(None);
    };
    if kind == TcpKind::Pair {
        info!(%remote, "closing a pairing connection, pairing is not available yet");
        return Ok(None);
    }
    let Secured {
        remote: key,
        stream,
        flushed,
        ..
    } = noise::respond(stream, &cluster.noise_key(), kind).await?;
    debug!(%remote, %key, "accepted a noise connection");
    let vouched_by = cluster.voucher(&key, remote.ip()).await;
    let handshake = cluster.noise_handshake(stream, key, vouched_by).await?;
    Ok(Some((handshake, flushed)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanOptions {
    pub enabled: bool,
    pub port: u16,
    pub state_dir: Option<PathBuf>,
}

pub struct LanListener {
    options: LanOptions,
    bound: watch::Sender<Option<SocketAddr>>,
    stop: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl LanListener {
    pub fn new(options: LanOptions) -> Self {
        Self {
            options,
            bound: watch::channel(None).0,
            stop: watch::channel(false).0,
            task: Mutex::new(None),
        }
    }

    pub fn start(&self, cluster: &Arc<Cluster>) {
        let task = tokio::spawn(keep_listening(
            Arc::clone(cluster),
            self.options.clone(),
            self.bound.clone(),
            self.stop.subscribe(),
        ));
        let replaced = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(task);
        if let Some(replaced) = replaced {
            replaced.abort();
        }
    }

    pub fn watch(&self) -> watch::Receiver<Option<SocketAddr>> {
        self.bound.subscribe()
    }

    pub async fn stop(&self) {
        self.stop.send_replace(true);
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

fn wants_lan_listener(cluster: &Cluster, options: &LanOptions) -> bool {
    options.enabled && cluster.trusts_anyone()
}

async fn keep_listening(
    cluster: Arc<Cluster>,
    options: LanOptions,
    bound: watch::Sender<Option<SocketAddr>>,
    mut stopping: watch::Receiver<bool>,
) {
    let port_file = options
        .state_dir
        .as_deref()
        .map(|dir| dir.join(LAN_PORT_FILE));
    remove_port_file(port_file.as_deref());
    let mut changes = cluster.watch();
    let mut listener: Option<Listener> = None;
    let mut attempted = false;
    loop {
        changes.borrow_and_update();
        if !wants_lan_listener(&cluster, &options) {
            attempted = false;
            if let Some(stopped) = listener.take() {
                info!(address = %stopped.local_addr(), "closed the LAN listener");
                unbound(&bound, port_file.as_deref());
            }
        } else if listener.is_none() && !attempted {
            attempted = true;
            let address = SocketAddr::from((Ipv4Addr::UNSPECIFIED, options.port));
            match Listener::bind(&cluster, address).await {
                Ok(started) => {
                    let local = started.local_addr();
                    info!(address = %local, "listening for peers on the LAN");
                    write_port_file(port_file.as_deref(), local.port());
                    bound.send_replace(Some(local));
                    listener = Some(started);
                }
                Err(err) => warn!("not listening for peers on the LAN: {err:#}"),
            }
        }
        tokio::select! {
            changed = changes.changed() => if changed.is_err() {
                break;
            },
            _ = stopping.wait_for(|stop| *stop) => break,
        }
    }
    if listener.take().is_some() {
        unbound(&bound, port_file.as_deref());
    }
}

fn unbound(bound: &watch::Sender<Option<SocketAddr>>, port_file: Option<&Path>) {
    bound.send_replace(None);
    remove_port_file(port_file);
}

fn write_port_file(path: Option<&Path>, port: u16) {
    let Some(path) = path else {
        return;
    };
    let temporary = path.with_extension("tmp");
    let written = fs::write(&temporary, format!("{port}\n"))
        .with_context(|| format!("writing {}", temporary.display()))
        .and_then(|()| {
            fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
        });
    if let Err(err) = written {
        warn!("recording the LAN port failed: {err:#}");
    }
}

fn remove_port_file(path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => warn!("removing {} failed: {err}", path.display()),
    }
}
