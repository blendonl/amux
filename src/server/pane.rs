use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use portable_pty::{
    native_pty_system, Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize,
};

use super::layout::PaneId;
use crate::protocol::{is_locale_variable, Size};
use crate::settings::PaneSettings;

const READ_BUFFER_LEN: usize = 16 * 1024;

pub trait PaneObserver: Send + Sync {
    fn pane_output(&self, pane: PaneId);
    fn pane_exited(&self, pane: PaneId, exit: Option<ExitStatus>);
}

pub struct PaneSpec<'a> {
    pub id: PaneId,
    pub cwd: &'a Path,
    pub size: Size,
    pub env: &'a [(String, String)],
    pub settings: &'a PaneSettings,
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
            .spawn_command(shell_command(spec.settings, spec.cwd, spec.env)?)
            .context("spawning the shell")?;
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let killer = child.clone_killer();
        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            size.rows,
            size.cols,
            spec.settings.scrollback,
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

fn shell_command(
    settings: &PaneSettings,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<CommandBuilder> {
    let mut command = match settings.shell.as_deref() {
        None | Some([_]) => CommandBuilder::new_default_prog(),
        Some([]) => bail!("pane.shell must name a program"),
        Some(argv) => CommandBuilder::from_argv(argv.iter().map(OsString::from).collect()),
    };
    let stripped: Vec<String> = command
        .iter_full_env_as_str()
        .map(|(key, _)| key)
        .filter(|key| settings.strips(key) || is_locale_variable(key))
        .map(str::to_owned)
        .collect();
    for key in stripped {
        command.env_remove(key);
    }
    for (key, value) in &settings.env {
        command.env(key, value);
    }
    for (key, value) in env.iter().filter(|(key, _)| is_locale_variable(key)) {
        command.env(key, value);
    }
    command.cwd(cwd);
    command.env("TERM", &settings.term);
    if let Some([program]) = settings.shell.as_deref() {
        let shell = find_program(program, command.get_env("PATH"))?;
        command.env("SHELL", shell);
    }
    Ok(command)
}

fn find_program(program: &str, path: Option<&OsStr>) -> Result<PathBuf> {
    let found = if program.contains('/') {
        Some(PathBuf::from(program)).filter(|candidate| is_executable(candidate))
    } else {
        path.into_iter()
            .flat_map(env::split_paths)
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
    };
    found.with_context(|| format!("can't start the shell {program}: no such executable"))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
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
        let exit = child.wait().ok();
        if let Some(observer) = sinks.observer.upgrade() {
            observer.pane_exited(sinks.pane, exit);
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use super::*;

    struct Quiet;

    impl PaneObserver for Quiet {
        fn pane_output(&self, _: PaneId) {}
        fn pane_exited(&self, _: PaneId, _: Option<ExitStatus>) {}
    }

    fn command(settings: &PaneSettings, env: &[(String, String)]) -> CommandBuilder {
        shell_command(settings, Path::new("/"), env).unwrap()
    }

    fn with_shell(argv: &[&str]) -> PaneSettings {
        PaneSettings {
            shell: Some(argv.iter().map(|arg| (*arg).to_owned()).collect()),
            ..PaneSettings::default()
        }
    }

    fn screen_text(pane: &Pane) -> String {
        pane.with_screen(|screen| screen.contents())
    }

    fn wait_for(pane: &Pane, wanted: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = screen_text(pane);
            if text.contains(wanted) || Instant::now() > deadline {
                return text;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_default_command_is_the_login_shell_with_the_default_term() {
        let command = command(&PaneSettings::default(), &[]);
        assert!(command.is_default_prog());
        assert_eq!(command.get_env("TERM"), Some(OsStr::new("screen-256color")));
        assert_eq!(command.get_cwd(), Some(&OsString::from("/")));
        assert!(command
            .iter_full_env_as_str()
            .all(|(key, _)| !key.starts_with("SSH_")));
    }

    #[test]
    fn the_command_takes_the_term_and_extra_environment() {
        let settings = PaneSettings {
            term: "tmux-256color".into(),
            env: BTreeMap::from([
                ("AMUX_EDITOR".into(), "vi".into()),
                ("LANG".into(), "C".into()),
            ]),
            ..PaneSettings::default()
        };
        let client = [
            ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
            ("AMUX_CLIENT_ONLY".to_owned(), "yes".to_owned()),
        ];
        let command = command(&settings, &client);

        assert_eq!(command.get_env("TERM"), Some(OsStr::new("tmux-256color")));
        assert_eq!(command.get_env("AMUX_EDITOR"), Some(OsStr::new("vi")));
        assert_eq!(command.get_env("LANG"), Some(OsStr::new("en_US.UTF-8")));
        assert_eq!(command.get_env("AMUX_CLIENT_ONLY"), None);
    }

    #[test]
    fn strip_env_removes_the_named_prefixes() {
        assert!(command(&PaneSettings::default(), &[])
            .get_env("CARGO_PKG_NAME")
            .is_some());
        let settings = PaneSettings {
            strip_env: vec!["CARGO_PKG_".into()],
            env: BTreeMap::from([("CARGO_PKG_KEPT".into(), "1".into())]),
            ..PaneSettings::default()
        };
        let command = command(&settings, &[]);
        assert_eq!(command.get_env("CARGO_PKG_NAME"), None);
        assert_eq!(command.get_env("CARGO_PKG_KEPT"), Some(OsStr::new("1")));
    }

    #[test]
    fn a_configured_shell_starts_as_a_login_shell() {
        let by_name = command(&with_shell(&["sh"]), &[]);
        assert!(by_name.is_default_prog());
        let shell = PathBuf::from(by_name.get_env("SHELL").unwrap());
        assert!(shell.is_absolute() && shell.ends_with("sh"), "{shell:?}");

        let by_path = command(&with_shell(&["/bin/sh"]), &[]);
        assert!(by_path.is_default_prog());
        assert_eq!(by_path.get_env("SHELL"), Some(OsStr::new("/bin/sh")));

        let observer: Weak<dyn PaneObserver> = Weak::<Quiet>::new();
        let pane = Pane::spawn(PaneSpec {
            id: PaneId(0),
            cwd: Path::new("/"),
            size: Size { rows: 10, cols: 40 },
            env: &[],
            settings: &with_shell(&["/bin/sh"]),
            observer,
        })
        .unwrap();
        pane.write_input(b"echo \"[$0]\"\r".to_vec()).unwrap();
        let text = wait_for(&pane, "[-sh]");
        assert!(text.contains("[-sh]"), "{text}");
    }

    #[test]
    fn a_shell_with_arguments_runs_as_given() {
        let command = command(&with_shell(&["/bin/sh", "-c", "exit"]), &[]);
        assert!(!command.is_default_prog());
        assert_eq!(command.get_argv(), &["/bin/sh", "-c", "exit"]);
    }

    #[test]
    fn a_missing_shell_is_reported() {
        let error = shell_command(&with_shell(&["no-such-shell-here"]), Path::new("/"), &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("no-such-shell-here"), "{error}");
        assert!(shell_command(&with_shell(&[]), Path::new("/"), &[]).is_err());
    }
}
