use std::env;
use std::process::{self, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;
use tracing::info;

use crate::client::{self, Endpoint};
use crate::settings::SshSettings;

pub const BRIDGE_EXIT_GRACE: Duration = Duration::from_secs(2);
pub const NOT_INSTALLED: &str = "amux is not installed on this machine";
const SSH_ENV: &str = "AMUX_SSH";
const BRIDGE_COMMAND: &str = "bridge";
const NO_START_FLAG: &str = "--no-start";
const AMUX_LOCATIONS: [&str; 4] = [
    "\"$HOME/.cargo/bin/amux\"",
    "\"$HOME/.local/bin/amux\"",
    "/usr/local/bin/amux",
    "/opt/homebrew/bin/amux",
];
const NOT_INSTALLED_STATUS: u8 = 127;

pub fn command(
    ssh: &SshSettings,
    user: Option<&str>,
    host: &str,
    port: Option<u16>,
    amux_path: Option<&str>,
    socket: &str,
    no_start: bool,
) -> Vec<String> {
    let mut argv = vec![program(env::var(SSH_ENV).ok(), &ssh.program)];
    argv.extend(ssh.options.iter().cloned());
    if let Some(port) = port {
        argv.extend(["-p".to_owned(), port.to_string()]);
    }
    argv.push(match user {
        Some(user) => format!("{user}@{host}"),
        None => host.to_owned(),
    });
    match amux_path {
        Some(amux_path) => {
            argv.extend([
                amux_path.to_owned(),
                "-L".to_owned(),
                shell_words::quote(socket).into_owned(),
                BRIDGE_COMMAND.to_owned(),
            ]);
            if no_start {
                argv.push(NO_START_FLAG.to_owned());
            }
        }
        None => argv.push(format!(
            "sh -c {}",
            shell_words::quote(&probe(&ssh.default_amux_path, socket, no_start))
        )),
    }
    argv
}

pub fn exec(argv: &[String], no_start: bool) -> Vec<String> {
    let mut argv = argv.to_vec();
    if no_start {
        argv.push(NO_START_FLAG.to_owned());
    }
    argv
}

fn program(overridden: Option<String>, configured: &str) -> String {
    overridden
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| configured.to_owned())
}

fn probe(amux: &str, socket: &str, no_start: bool) -> String {
    let mut bridge = format!("-L {} {BRIDGE_COMMAND}", shell_words::quote(socket));
    if no_start {
        bridge.push(' ');
        bridge.push_str(NO_START_FLAG);
    }
    format!(
        "command -v {amux} >/dev/null 2>&1 && exec {amux} {bridge}; \
         for amux in {}; do [ -x \"$amux\" ] && exec \"$amux\" {bridge}; done; \
         echo '{NOT_INSTALLED}' >&2; exit {NOT_INSTALLED_STATUS}",
        AMUX_LOCATIONS.join(" ")
    )
}

pub struct Transport {
    pub child: Child,
    pub reader: ChildStdout,
    pub writer: ChildStdin,
    pub stderr: StderrTail,
}

pub struct StderrTail(Option<JoinHandle<Option<String>>>);

pub fn spawn(argv: &[String], address: &str) -> Result<Transport> {
    let (program, args) = argv.split_first().context("the bridge command is empty")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("running {program}"))?;

    let reader = child.stdout.take().context("the bridge has no stdout")?;
    let writer = child.stdin.take().context("the bridge has no stdin")?;
    let stderr = child.stderr.take().map(|stderr| {
        let address = address.to_owned();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut last = None;
            while let Ok(Some(line)) = lines.next_line().await {
                info!(%address, "bridge: {line}");
                if !line.trim().is_empty() {
                    last = Some(line);
                }
            }
            last
        })
    });
    Ok(Transport {
        child,
        reader,
        writer,
        stderr: StderrTail(stderr),
    })
}

pub async fn last_words(child: &mut Child, stderr: StderrTail) -> Option<String> {
    if tokio::time::timeout(BRIDGE_EXIT_GRACE, child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
    }
    let reader = stderr.0?;
    tokio::time::timeout(BRIDGE_EXIT_GRACE, reader)
        .await
        .ok()?
        .ok()?
}

pub async fn bridge(endpoint: &Endpoint, no_start: bool) -> Result<()> {
    let stream = if no_start {
        client::connect_stream(&endpoint.socket).await?
    } else {
        client::connect_or_start_server(endpoint).await?
    };
    let (mut from_server, mut to_server) = stream.into_split();

    let upstream = async move {
        let copied = io::copy(&mut io::stdin(), &mut to_server).await;
        let _ = to_server.shutdown().await;
        copied
    };
    let downstream = async move {
        let mut stdout = io::stdout();
        let copied = io::copy(&mut from_server, &mut stdout).await;
        let _ = stdout.flush().await;
        copied
    };

    let ended = tokio::select! {
        ended = upstream => ended,
        ended = downstream => ended,
    };
    match ended {
        Ok(_) => process::exit(0),
        Err(err) => {
            eprintln!("amux bridge: {err}");
            process::exit(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Output;

    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    fn fake_amux(path: &Path, marker: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("#!/bin/sh\necho {marker} \"$@\"\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run_remotely(remote: &str, home: &Path, path: &str) -> Output {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(remote)
            .env_clear()
            .env("HOME", home)
            .env("PATH", path)
            .output()
            .unwrap()
    }

    fn remote_command(socket: &str, no_start: bool) -> String {
        command(
            &SshSettings::default(),
            None,
            "laptop",
            None,
            None,
            socket,
            no_start,
        )
        .pop()
        .unwrap()
    }

    #[test]
    fn without_an_amux_path_ssh_runs_one_quoted_probe() {
        let argv = command(
            &SshSettings::default(),
            None,
            "laptop",
            None,
            None,
            "default",
            false,
        );
        assert_eq!(
            argv[..argv.len() - 1],
            [
                "ssh",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=15",
                "laptop",
            ]
        );
        let remote = argv.last().unwrap();
        let words = shell_words::split(remote).unwrap();
        assert_eq!(words[..2], ["sh", "-c"]);
        assert_eq!(words.len(), 3, "{remote}");
        assert!(
            words[2].contains("exec amux -L default bridge;"),
            "{remote}"
        );
        assert!(words[2].contains("$HOME/.cargo/bin/amux"), "{remote}");
        assert!(words[2].contains("/opt/homebrew/bin/amux"), "{remote}");
        assert!(words[2].ends_with("exit 127"), "{remote}");
    }

    #[test]
    fn an_amux_path_is_passed_to_the_remote_shell_as_written() {
        assert_eq!(
            command(
                &SshSettings::default(),
                Some("notpc"),
                "home-server",
                Some(2222),
                Some("~/.cargo/bin/amux"),
                "dev box",
                true
            ),
            argv(&[
                "ssh",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=15",
                "-p",
                "2222",
                "notpc@home-server",
                "~/.cargo/bin/amux",
                "-L",
                "'dev box'",
                "bridge",
                "--no-start",
            ])
        );
    }

    #[test]
    fn the_probe_finds_amux_outside_the_path() {
        let home = tempfile::tempdir().unwrap();
        fake_amux(&home.path().join(".local/bin/amux"), "local");

        let output = run_remotely(
            &remote_command("dev box", true),
            home.path(),
            "/usr/bin:/bin",
        );

        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "local -L dev box bridge --no-start\n"
        );
    }

    #[test]
    fn the_probe_prefers_amux_on_the_path_then_cargo_bin() {
        let home = tempfile::tempdir().unwrap();
        let on_path = home.path().join("bin");
        fake_amux(&on_path.join("amux"), "path");
        fake_amux(&home.path().join(".cargo/bin/amux"), "cargo");
        fake_amux(&home.path().join(".local/bin/amux"), "local");
        let remote = remote_command("default", false);

        let output = run_remotely(
            &remote,
            home.path(),
            &format!("{}:/usr/bin:/bin", on_path.display()),
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "path -L default bridge\n"
        );

        let output = run_remotely(&remote, home.path(), "/usr/bin:/bin");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "cargo -L default bridge\n"
        );
    }

    #[test]
    fn the_ssh_program_options_and_amux_path_come_from_the_settings() {
        let ssh = SshSettings {
            program: "autossh".into(),
            options: argv(&["-M", "0", "-o", "BatchMode=yes"]),
            default_amux_path: "\"$HOME/custom/amux\"".into(),
        };

        assert_eq!(
            command(&ssh, None, "laptop", Some(2222), Some("amux"), "dev", false),
            argv(&[
                "autossh",
                "-M",
                "0",
                "-o",
                "BatchMode=yes",
                "-p",
                "2222",
                "laptop",
                "amux",
                "-L",
                "dev",
                "bridge",
            ])
        );

        let remote = command(&ssh, None, "laptop", None, None, "default", false)
            .pop()
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_amux(&home.path().join("custom/amux"), "custom");
        fake_amux(&home.path().join(".cargo/bin/amux"), "cargo");
        let output = run_remotely(&remote, home.path(), "/usr/bin:/bin");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "custom -L default bridge\n"
        );
    }

    #[test]
    fn the_ssh_environment_variable_wins_over_the_settings() {
        assert_eq!(program(Some("fake-ssh".into()), "autossh"), "fake-ssh");
        assert_eq!(program(Some(String::new()), "autossh"), "autossh");
        assert_eq!(program(None, "autossh"), "autossh");
    }

    #[test]
    fn the_probe_says_when_amux_is_missing() {
        let system_wide = ["/usr/local/bin/amux", "/opt/homebrew/bin/amux"];
        if system_wide.iter().any(|path| Path::new(path).exists()) {
            return;
        }
        let home = tempfile::tempdir().unwrap();

        let output = run_remotely(
            &remote_command("default", false),
            home.path(),
            "/usr/bin:/bin",
        );

        assert_eq!(output.status.code(), Some(i32::from(NOT_INSTALLED_STATUS)));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("{NOT_INSTALLED}\n")
        );
    }

    #[tokio::test]
    async fn the_last_stderr_line_of_a_failed_bridge_is_kept() {
        let script = "echo starting >&2; echo 'no server running' >&2; echo >&2; exit 1";
        let Transport {
            mut child, stderr, ..
        } = spawn(&argv(&["sh", "-c", script]), "exec:test").unwrap();

        assert_eq!(
            last_words(&mut child, stderr).await.as_deref(),
            Some("no server running")
        );
    }
}
