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

use super::graphics::place::Placements;
use super::graphics::store::{ImageStore, PaneImages};
use super::graphics::PaneGraphics;
use super::layout::PaneId;
use super::replies::PaneCallbacks;
use crate::protocol::{is_locale_variable, CellPixels, ClientTerminal, Size};
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
    pub store: &'a Arc<ImageStore>,
    pub terminal: Option<ClientTerminal>,
}

pub struct Pane {
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: mpsc::Sender<Vec<u8>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    parser: Arc<Mutex<vt100::Parser<PaneCallbacks>>>,
    images: Option<PaneImages>,
}

impl Pane {
    pub fn spawn(spec: PaneSpec<'_>) -> Result<Self> {
        check_working_directory(spec.cwd)?;
        let size = spec.size.clamped();
        let pair = native_pty_system()
            .openpty(pty_size(size, None))
            .context("opening a pty")?;

        let input = spawn_input_pump(pair.master.take_writer()?);
        let images = spec.settings.images.then(|| spec.store.open_pane());
        let callbacks = PaneCallbacks::new(input.clone(), images.clone());
        let terminal = spec.terminal.unwrap_or_default();
        callbacks.set_cell_pixels(terminal.cell_pixels);
        callbacks.set_graphics(terminal.graphics && spec.settings.images);
        if let Some(pixels) = callbacks.cell_pixels() {
            pair.master.resize(pty_size(size, Some(pixels)))?;
        }

        let child = pair
            .slave
            .spawn_command(shell_command(spec.settings, spec.cwd, spec.env)?)
            .context("spawning the shell")?;
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let killer = child.clone_killer();
        let graphics = images.clone().map(PaneGraphics::new);
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            size.rows,
            size.cols,
            spec.settings.scrollback,
            callbacks,
        )));

        spawn_output_pump(
            reader,
            child,
            OutputSinks {
                pane: spec.id,
                parser: Arc::clone(&parser),
                observer: spec.observer,
                graphics,
            },
        );

        Ok(Self {
            master: Mutex::new(pair.master),
            input,
            killer: Mutex::new(killer),
            parser,
            images,
        })
    }

    pub fn kill(&self) {
        let _ = lock(&self.killer).kill();
    }

    pub fn with_screen<R>(&self, read: impl FnOnce(&vt100::Screen) -> R) -> R {
        read(lock(&self.parser).screen())
    }

    pub fn with_pane<R>(&self, read: impl FnOnce(&vt100::Screen, Option<&Placements>) -> R) -> R {
        let parser = lock(&self.parser);
        read(parser.screen(), parser.callbacks().placements())
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
        let master = lock(&self.master);
        master.resize(pty_size(size, self.cell_pixels()))?;
        let mut parser = lock(&self.parser);
        let (screen, callbacks) = parser.parts_mut();
        screen.set_size(size.rows, size.cols);
        if let Some(placements) = callbacks.placements_mut() {
            placements.settle(screen);
        }
        Ok(())
    }

    pub fn set_cell_pixels(&self, pixels: Option<CellPixels>) -> Result<()> {
        let master = lock(&self.master);
        let parser = lock(&self.parser);
        if !parser.callbacks().set_cell_pixels(pixels) {
            return Ok(());
        }
        let (rows, cols) = parser.screen().size();
        let pixels = parser.callbacks().cell_pixels();
        drop(parser);
        master.resize(pty_size(Size { rows, cols }, pixels))?;
        Ok(())
    }

    pub fn set_graphics(&self, graphics: bool) {
        lock(&self.parser)
            .callbacks()
            .set_graphics(graphics && self.images.is_some());
    }

    fn cell_pixels(&self) -> Option<CellPixels> {
        lock(&self.parser).callbacks().cell_pixels()
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.kill();
        if let Some(images) = &self.images {
            images.close();
        }
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
    parser: Arc<Mutex<vt100::Parser<PaneCallbacks>>>,
    observer: Weak<dyn PaneObserver>,
    graphics: Option<PaneGraphics>,
}

impl OutputSinks {
    fn process(&mut self, output: &[u8]) {
        match &mut self.graphics {
            Some(graphics) => graphics.process(output, &self.parser),
            None => lock(&self.parser).process(output),
        }
    }
}

fn spawn_output_pump(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn Child + Send + Sync>,
    mut sinks: OutputSinks,
) {
    thread::spawn(move || {
        let mut buffer = vec![0; READ_BUFFER_LEN];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(len) => {
                    sinks.process(&buffer[..len]);
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

fn pty_size(size: Size, cell_pixels: Option<CellPixels>) -> PtySize {
    let (width, height) = cell_pixels.map_or((0, 0), |pixels| (pixels.width, pixels.height));
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.cols.saturating_mul(width),
        pixel_height: size.rows.saturating_mul(height),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use super::super::graphics::store::{Buffer, Name};
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

    fn spawn_pane(settings: &PaneSettings, size: Size) -> Pane {
        spawn_pane_for(settings, size, None)
    }

    fn spawn_pane_for(
        settings: &PaneSettings,
        size: Size,
        terminal: Option<ClientTerminal>,
    ) -> Pane {
        let observer: Weak<dyn PaneObserver> = Weak::<Quiet>::new();
        Pane::spawn(PaneSpec {
            id: PaneId(0),
            cwd: Path::new("/"),
            size,
            env: &[],
            settings,
            observer,
            store: &Arc::new(ImageStore::new(1 << 20)),
            terminal,
        })
        .unwrap()
    }

    fn screen_text(pane: &Pane) -> String {
        pane.with_screen(|screen| screen.contents())
    }

    fn spans(pane: &Pane) -> Vec<(u16, u16)> {
        let parser = lock(&pane.parser);
        let placements = parser.callbacks().placements().unwrap();
        placements
            .spans(parser.screen())
            .map(|span| (span.row, span.image_row))
            .collect()
    }

    fn placed(pane: &Pane) -> bool {
        !lock(&pane.parser)
            .callbacks()
            .placements()
            .unwrap()
            .is_empty()
    }

    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if done() {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
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

        let pane = spawn_pane(&with_shell(&["/bin/sh"]), Size { rows: 10, cols: 40 });
        pane.write_input(b"echo \"[$0]\"\r".to_vec()).unwrap();
        let text = wait_for(&pane, "[-sh]");
        assert!(text.contains("[-sh]"), "{text}");
    }

    #[test]
    fn a_program_reads_the_cursor_position_it_asks_for() {
        let script = r#"printf '\033[6n'; read -r reply; printf 'got%s' "${reply#?}""#;
        let pane = spawn_pane(
            &with_shell(&["/bin/sh", "-c", script]),
            Size { rows: 10, cols: 40 },
        );
        let echoed = wait_for(&pane, "^[[1;1R");
        assert!(echoed.contains("^[[1;1R"), "{echoed}");
        pane.write_input(b"\r".to_vec()).unwrap();
        let text = wait_for(&pane, "got[1;1R");
        assert!(text.contains("got[1;1R"), "{text}");
    }

    #[test]
    fn the_pty_size_carries_pixels_once_the_cell_size_is_known() {
        let pane = spawn_pane(&with_shell(&["/bin/sh"]), Size { rows: 10, cols: 40 });
        let pty = |rows, cols, pixel_width, pixel_height| {
            assert_eq!(
                lock(&pane.master).get_size().unwrap(),
                PtySize {
                    rows,
                    cols,
                    pixel_width,
                    pixel_height
                }
            );
        };
        pty(10, 40, 0, 0);

        let cell = CellPixels {
            width: 9,
            height: 18,
        };
        pane.set_cell_pixels(Some(cell)).unwrap();
        pty(10, 40, 360, 180);
        pane.resize(Size { rows: 12, cols: 50 }).unwrap();
        pty(12, 50, 450, 216);
        pane.resize(Size { rows: 12, cols: 50 }).unwrap();
        pty(12, 50, 450, 216);
        pane.set_cell_pixels(None).unwrap();
        pty(12, 50, 0, 0);
    }

    #[test]
    fn the_graphics_flag_reaches_the_responder() {
        let pane = spawn_pane(&with_shell(&["/bin/sh"]), Size { rows: 10, cols: 40 });
        assert!(!lock(&pane.parser).callbacks().graphics());
        pane.set_graphics(true);
        assert!(lock(&pane.parser).callbacks().graphics());
    }

    const QUERY_THEN_DEVICE_ATTRIBUTES: &str =
        r#"read -r go; printf '\033_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\033\\\033[c'; read -r reply"#;

    #[test]
    fn a_kitty_query_is_answered_before_the_device_attributes_that_follow_it() {
        let pane = spawn_pane(
            &with_shell(&["/bin/sh", "-c", QUERY_THEN_DEVICE_ATTRIBUTES]),
            Size { rows: 10, cols: 60 },
        );
        pane.set_graphics(true);
        pane.write_input(b"go\r".to_vec()).unwrap();
        let echoed = wait_for(&pane, "^[[?62;22c");
        assert!(echoed.contains("^[_Gi=31;OK^[\\^[[?62;22c"), "{echoed}");
    }

    #[test]
    fn a_kitty_query_goes_unanswered_while_the_client_shows_no_images() {
        for (images, graphics) in [(true, false), (false, true)] {
            let settings = PaneSettings {
                images,
                ..with_shell(&["/bin/sh", "-c", QUERY_THEN_DEVICE_ATTRIBUTES])
            };
            let pane = spawn_pane(&settings, Size { rows: 10, cols: 60 });
            pane.set_graphics(graphics);
            pane.write_input(b"go\r".to_vec()).unwrap();
            let echoed = wait_for(&pane, "^[[?62;22c");
            assert!(echoed.contains("go\n^[[?62;22c"), "{echoed}");
            assert!(!echoed.contains("OK"), "{echoed}");
        }
    }

    #[test]
    fn a_new_pane_starts_with_the_session_terminal() {
        let terminal = ClientTerminal {
            graphics: true,
            cell_pixels: Some(CellPixels {
                width: 9,
                height: 18,
            }),
        };
        let pane = spawn_pane_for(
            &with_shell(&["/bin/sh"]),
            Size { rows: 10, cols: 40 },
            Some(terminal),
        );
        let pty = lock(&pane.master).get_size().unwrap();
        assert_eq!((pty.pixel_width, pty.pixel_height), (360, 180));
        assert!(lock(&pane.parser).callbacks().graphics());

        let without_images = PaneSettings {
            images: false,
            ..with_shell(&["/bin/sh"])
        };
        let pane = spawn_pane_for(&without_images, Size { rows: 10, cols: 40 }, Some(terminal));
        assert!(!lock(&pane.parser).callbacks().graphics());
        pane.set_graphics(true);
        assert!(!lock(&pane.parser).callbacks().graphics());
    }

    #[test]
    fn a_pane_keeps_the_images_it_is_sent_until_it_closes() {
        let script = r#"printf '\033_Gi=4,q=2,f=24,s=1,v=1;AAAA\033\\ready'; read -r line"#;
        let pane = spawn_pane(
            &with_shell(&["/bin/sh", "-c", script]),
            Size { rows: 10, cols: 40 },
        );
        let images = pane.images.clone().unwrap();
        let text = wait_for(&pane, "ready");
        assert!(text.starts_with("ready"), "{text}");
        assert!(images.lookup(Buffer::Main, Name::Id(4)).is_some());
        drop(pane);
        assert!(images.lookup(Buffer::Main, Name::Id(4)).is_none());

        let ignored = PaneSettings {
            images: false,
            ..with_shell(&["/bin/sh", "-c", script])
        };
        let pane = spawn_pane(&ignored, Size { rows: 10, cols: 40 });
        assert!(pane.images.is_none());
        let text = wait_for(&pane, "ready");
        assert!(text.starts_with("ready"), "{text}");
    }

    #[test]
    fn an_image_scrolls_away_with_the_text_under_it() {
        let script = r#"printf 'top\n\033_Ga=T,i=1,q=2,c=4,r=3,f=24,s=1,v=1;AAAA\033\\'; read -r go; seq 1 7; read -r go; seq 1 100; read -r go"#;
        let pane = spawn_pane(
            &with_shell(&["/bin/sh", "-c", script]),
            Size { rows: 10, cols: 40 },
        );
        let shown = wait_until(|| spans(&pane) == [(1, 0), (2, 1), (3, 2)]);
        assert!(shown, "{:?}", spans(&pane));
        pane.write_input(b"\r".to_vec()).unwrap();
        let moved = wait_until(|| spans(&pane) == [(0, 1), (1, 2)]);
        assert!(moved, "{:?}", spans(&pane));
        pane.write_input(b"\r".to_vec()).unwrap();
        assert!(wait_until(|| !placed(&pane)), "{:?}", spans(&pane));
        assert!(spans(&pane).is_empty());
        assert!(screen_text(&pane).contains("100"));
    }

    #[test]
    fn resizing_a_pane_cuts_the_image_rows_it_drops() {
        let script = r#"printf '\033[5;1H\033_Ga=T,i=1,q=2,C=1,c=2,r=2,f=24,s=1,v=1;AAAA\033\\ready'; read -r go"#;
        let pane = spawn_pane(
            &with_shell(&["/bin/sh", "-c", script]),
            Size { rows: 10, cols: 40 },
        );
        let shown = wait_until(|| spans(&pane) == [(4, 0), (5, 1)]);
        assert!(shown, "{:?}", spans(&pane));
        pane.resize(Size { rows: 5, cols: 40 }).unwrap();
        assert_eq!(spans(&pane), [(4, 0)]);
        pane.resize(Size { rows: 4, cols: 40 }).unwrap();
        assert!(spans(&pane).is_empty());
        assert!(!placed(&pane));
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
