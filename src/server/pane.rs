use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tokio::sync::watch;

use crate::protocol::Size;

const SCROLLBACK_LINES: usize = 10_000;
const READ_BUFFER_LEN: usize = 16 * 1024;
const PANE_TERM: &str = "screen-256color";
const STRIPPED_ENV_PREFIX: &str = "SSH_";

pub struct Pane {
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    parser: Arc<Mutex<vt100::Parser>>,
    updates: watch::Receiver<()>,
}

impl Pane {
    pub fn spawn(cwd: &Path, size: Size) -> Result<Self> {
        check_working_directory(cwd)?;
        let pair = native_pty_system()
            .openpty(pty_size(size))
            .context("opening a pty")?;

        let child = pair
            .slave
            .spawn_command(shell_command(cwd))
            .context("spawning the shell")?;
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let killer = child.clone_killer();
        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            size.rows,
            size.cols,
            SCROLLBACK_LINES,
        )));
        let (notifier, updates) = watch::channel(());

        spawn_output_pump(reader, child, Arc::clone(&parser), notifier);

        Ok(Self {
            master: Mutex::new(pair.master),
            input: spawn_input_pump(writer),
            killer,
            parser,
            updates,
        })
    }

    pub fn subscribe(&self) -> watch::Receiver<()> {
        let mut updates = self.updates.clone();
        updates.borrow_and_update();
        updates
    }

    pub fn screen(&self) -> vt100::Screen {
        lock(&self.parser).screen().clone()
    }

    pub fn write_input(&self, bytes: Vec<u8>) -> Result<()> {
        self.input
            .send(bytes)
            .map_err(|_| anyhow!("the pane is no longer accepting input"))
    }

    pub fn resize(&self, size: Size) -> Result<()> {
        lock(&self.master).resize(pty_size(size))?;
        lock(&self.parser)
            .screen_mut()
            .set_size(size.rows, size.cols);
        Ok(())
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}

fn check_working_directory(cwd: &Path) -> Result<()> {
    let metadata =
        fs::metadata(cwd).with_context(|| format!("can't start a shell in {}", cwd.display()))?;
    if !metadata.is_dir() {
        bail!("can't start a shell in {}: not a directory", cwd.display());
    }
    Ok(())
}

fn shell_command(cwd: &Path) -> CommandBuilder {
    let mut command = CommandBuilder::new_default_prog();
    let stripped: Vec<String> = command
        .iter_full_env_as_str()
        .map(|(key, _)| key)
        .filter(|key| key.starts_with(STRIPPED_ENV_PREFIX))
        .map(str::to_owned)
        .collect();
    for key in stripped {
        command.env_remove(key);
    }
    command.cwd(cwd);
    command.env("TERM", PANE_TERM);
    command
}

fn spawn_output_pump(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn Child + Send + Sync>,
    parser: Arc<Mutex<vt100::Parser>>,
    notifier: watch::Sender<()>,
) {
    thread::spawn(move || {
        let mut buffer = vec![0; READ_BUFFER_LEN];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(len) => {
                    lock(&parser).process(&buffer[..len]);
                    notifier.send_replace(());
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = child.wait();
    });
}

fn spawn_input_pump(mut writer: Box<dyn Write + Send>) -> mpsc::Sender<Vec<u8>> {
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        for bytes in receiver {
            if writer
                .write_all(&bytes)
                .and_then(|()| writer.flush())
                .is_err()
            {
                break;
            }
        }
    });
    sender
}

fn pty_size(size: Size) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
