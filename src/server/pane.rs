use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

use super::layout::PaneId;
use crate::protocol::{is_locale_variable, Size};

const SCROLLBACK_LINES: usize = 10_000;
const READ_BUFFER_LEN: usize = 16 * 1024;
const PANE_TERM: &str = "screen-256color";
const STRIPPED_ENV_PREFIX: &str = "SSH_";

pub trait PaneObserver: Send + Sync {
    fn pane_output(&self, pane: PaneId);
    fn pane_exited(&self, pane: PaneId);
}

pub struct PaneSpec<'a> {
    pub id: PaneId,
    pub cwd: &'a Path,
    pub size: Size,
    pub env: &'a [(String, String)],
    pub observer: Weak<dyn PaneObserver>,
}

pub struct Pane {
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: mpsc::Sender<Vec<u8>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    parser: Arc<Mutex<vt100::Parser>>,
}

impl Pane {
    pub fn spawn(spec: PaneSpec<'_>) -> Result<Self> {
        check_working_directory(spec.cwd)?;
        let size = spec.size.clamped();
        let pair = native_pty_system()
            .openpty(pty_size(size))
            .context("opening a pty")?;

        let child = pair
            .slave
            .spawn_command(shell_command(spec.cwd, spec.env))
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

        spawn_output_pump(
            reader,
            child,
            OutputSinks {
                pane: spec.id,
                parser: Arc::clone(&parser),
                observer: spec.observer,
            },
        );

        Ok(Self {
            master: Mutex::new(pair.master),
            input: spawn_input_pump(writer),
            killer: Mutex::new(killer),
            parser,
        })
    }

    pub fn kill(&self) {
        let _ = lock(&self.killer).kill();
    }

    pub fn with_screen<R>(&self, read: impl FnOnce(&vt100::Screen) -> R) -> R {
        read(lock(&self.parser).screen())
    }

    pub fn write_input(&self, bytes: Vec<u8>) -> Result<()> {
        self.input
            .send(bytes)
            .map_err(|_| anyhow!("the pane is no longer accepting input"))
    }

    pub fn resize(&self, size: Size) -> Result<()> {
        let size = size.clamped();
        if self.with_screen(|screen| screen.size()) == (size.rows, size.cols) {
            return Ok(());
        }
        lock(&self.master).resize(pty_size(size))?;
        lock(&self.parser)
            .screen_mut()
            .set_size(size.rows, size.cols);
        Ok(())
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.kill();
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

fn shell_command(cwd: &Path, env: &[(String, String)]) -> CommandBuilder {
    let mut command = CommandBuilder::new_default_prog();
    let stripped: Vec<String> = command
        .iter_full_env_as_str()
        .map(|(key, _)| key)
        .filter(|key| key.starts_with(STRIPPED_ENV_PREFIX) || is_locale_variable(key))
        .map(str::to_owned)
        .collect();
    for key in stripped {
        command.env_remove(key);
    }
    for (key, value) in env.iter().filter(|(key, _)| is_locale_variable(key)) {
        command.env(key, value);
    }
    command.cwd(cwd);
    command.env("TERM", PANE_TERM);
    command
}

struct OutputSinks {
    pane: PaneId,
    parser: Arc<Mutex<vt100::Parser>>,
    observer: Weak<dyn PaneObserver>,
}

fn spawn_output_pump(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn Child + Send + Sync>,
    sinks: OutputSinks,
) {
    thread::spawn(move || {
        let mut buffer = vec![0; READ_BUFFER_LEN];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(len) => {
                    lock(&sinks.parser).process(&buffer[..len]);
                    if let Some(observer) = sinks.observer.upgrade() {
                        observer.pane_output(sinks.pane);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = child.wait();
        if let Some(observer) = sinks.observer.upgrade() {
            observer.pane_exited(sinks.pane);
        }
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
