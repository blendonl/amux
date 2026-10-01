mod common;

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use amux::protocol::{cli_version, PROTOCOL_MAJOR, PROTOCOL_MINOR, RELEASE};
use common::releases::{target, FakeReleases};
use common::{linked, TestServer, AMUX, TIMEOUT};
use tempfile::TempDir;

const NEWER: &str = "99.0.0";

struct InstalledAmux {
    dir: TempDir,
    path: PathBuf,
}

impl InstalledAmux {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("creating the install dir");
        let path = dir.path().join("amux");
        fs::copy(AMUX, &path).expect("copying amux");
        Self { dir, path }
    }

    fn update(&self, server: &TestServer, releases: &FakeReleases, args: &[&str]) -> Output {
        run(Command::new(&self.path)
            .env_clear()
            .envs(server.env().iter().map(|(key, value)| (key, value)))
            .env("AMUX_RELEASES_URL", releases.url())
            .arg("-S")
            .arg(server.socket())
            .arg("update")
            .args(args)
            .stdin(Stdio::null()))
    }

    fn version(&self) -> String {
        let output = run(Command::new(&self.path).arg("--version"));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.dir.path())
            .expect("listing the install dir")
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn assert_untouched(&self) {
        assert_eq!(self.version(), this_version());
        assert_eq!(self.files(), ["amux"]);
    }
}

fn run(command: &mut Command) -> Output {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match command.output() {
            Err(err)
                if err.kind() == io::ErrorKind::ExecutableFileBusy && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            result => return result.expect("running the installed amux"),
        }
    }
}

fn this_version() -> String {
    format!("amux {}", cli_version())
}

fn compatible() -> String {
    format!("{PROTOCOL_MAJOR}.{PROTOCOL_MINOR}")
}

fn incompatible() -> String {
    format!("{}.0", PROTOCOL_MAJOR + 1)
}

fn succeeded(output: &Output) -> String {
    assert!(
        output.status.success(),
        "amux update failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn failed(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "amux update succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn check_names_a_newer_release_and_installs_nothing() {
    let server = TestServer::builder().prepare();
    let releases = FakeReleases::new();
    releases.publish(NEWER, &compatible()).set_latest(NEWER);
    let amux = InstalledAmux::new();

    let stdout = succeeded(&amux.update(&server, &releases, &["--check"]));

    assert!(
        stdout.contains(&format!(
            "amux {NEWER} is out, this is amux {RELEASE}; run `amux update`"
        )),
        "{stdout}"
    );
    amux.assert_untouched();
}

#[test]
fn update_replaces_the_binary_with_the_latest_release() {
    let server = TestServer::builder().prepare();
    let releases = FakeReleases::new();
    releases.publish(NEWER, &compatible()).set_latest(NEWER);
    let amux = InstalledAmux::new();

    let stdout = succeeded(&amux.update(&server, &releases, &[]));

    let path = fs::canonicalize(&amux.path).unwrap();
    assert_eq!(
        stdout,
        format!(
            "downloading amux {NEWER} for {}\nupdated {} from amux {RELEASE} to amux {NEWER}\n",
            target(),
            path.display()
        )
    );
    assert_eq!(
        amux.version(),
        format!("amux {NEWER} (protocol {})", compatible())
    );
    assert_eq!(amux.files(), ["amux"]);
}

#[test]
fn an_update_refreshes_lua_types_that_config_lsp_wrote() {
    let server = TestServer::builder().prepare();
    let types = server.home().join(".local/share/amux/lua/amux.lua");
    fs::create_dir_all(types.parent().unwrap()).unwrap();
    fs::write(&types, "---@meta amux\n").unwrap();
    let ran = server.home().join("ran");
    let releases = FakeReleases::new();
    releases
        .publish_binary(
            NEWER,
            &format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = config ]; then echo \"$@\" > \"$HOME/ran\"; exit 0; fi\n\
                 echo 'amux {NEWER} (protocol {})'\n",
                compatible()
            ),
        )
        .set_latest(NEWER);
    let amux = InstalledAmux::new();

    let stdout = succeeded(&amux.update(&server, &releases, &[]));

    assert!(
        stdout.ends_with(&format!("updated the Lua types in {}\n", types.display())),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(ran).unwrap(), "config lsp\n");
}

#[test]
fn the_latest_release_leaves_this_build_alone() {
    let server = TestServer::builder().prepare();
    let releases = FakeReleases::new();
    releases.publish(RELEASE, &compatible()).set_latest(RELEASE);
    let amux = InstalledAmux::new();

    let stdout = succeeded(&amux.update(&server, &releases, &[]));

    assert_eq!(stdout, format!("amux {RELEASE} is the latest release\n"));
    amux.assert_untouched();
}

#[test]
fn a_pinned_older_release_is_installed() {
    let server = TestServer::builder().prepare();
    let releases = FakeReleases::new();
    releases
        .publish("0.0.1", &compatible())
        .publish(NEWER, &compatible())
        .set_latest(NEWER);
    let amux = InstalledAmux::new();

    succeeded(&amux.update(&server, &releases, &["v0.0.1"]));

    assert_eq!(
        amux.version(),
        format!("amux 0.0.1 (protocol {})", compatible())
    );
}

#[test]
fn a_download_with_the_wrong_checksum_installs_nothing() {
    let server = TestServer::builder().prepare();
    let releases = FakeReleases::new();
    releases
        .publish(NEWER, &compatible())
        .set_latest(NEWER)
        .corrupt(NEWER);
    let amux = InstalledAmux::new();

    let stderr = failed(&amux.update(&server, &releases, &[]));

    assert!(
        stderr.contains("does not match its checksum, so nothing was installed"),
        "{stderr}"
    );
    amux.assert_untouched();
}

#[test]
fn a_release_that_is_not_published_installs_nothing() {
    let server = TestServer::builder().prepare();
    let releases = FakeReleases::new();
    let amux = InstalledAmux::new();

    let stderr = failed(&amux.update(&server, &releases, &["5.5.5"]));

    assert!(stderr.contains("downloading file://"), "{stderr}");
    assert!(stderr.contains("/download/v5.5.5/"), "{stderr}");
    amux.assert_untouched();
}

#[test]
fn a_download_that_does_not_run_installs_nothing() {
    let server = TestServer::builder().prepare();
    let releases = FakeReleases::new();
    releases
        .publish_binary(NEWER, "#!/bin/sh\necho 'Exec format error' >&2\nexit 126\n")
        .set_latest(NEWER);
    let amux = InstalledAmux::new();

    let stderr = failed(&amux.update(&server, &releases, &[]));

    assert!(
        stderr.contains(&format!(
            "the amux {NEWER} download does not run on this machine: Exec format error"
        )),
        "{stderr}"
    );
    amux.assert_untouched();
}

#[test]
fn a_download_of_another_release_installs_nothing() {
    let server = TestServer::builder().prepare();
    let releases = FakeReleases::new();
    releases.publish_binary(NEWER, "#!/bin/sh\necho 'amux 98.0.0 (protocol 9.0)'\n");
    let amux = InstalledAmux::new();

    let stderr = failed(&amux.update(&server, &releases, &[NEWER]));

    assert!(
        stderr.contains(&format!(
            "the download for amux {NEWER} is amux 98.0.0 (protocol 9.0)"
        )),
        "{stderr}"
    );
    amux.assert_untouched();
}

#[test]
fn a_running_server_keeps_going_after_a_compatible_update() {
    let server = TestServer::start();
    let releases = FakeReleases::new();
    releases.publish(NEWER, &compatible()).set_latest(NEWER);
    let amux = InstalledAmux::new();

    let stdout = succeeded(&amux.update(&server, &releases, &[]));

    assert!(
        stdout.contains(&format!(
            "the server `{}` still runs amux {RELEASE} (protocol {}) and keeps its sessions, \
             and amux {NEWER} can talk to it",
            server.name(),
            compatible()
        )),
        "{stdout}"
    );
    assert!(server.is_listening());
}

#[test]
fn an_incompatible_update_says_to_stop_the_server() {
    let server = TestServer::start();
    let releases = FakeReleases::new();
    releases.publish(NEWER, &incompatible()).set_latest(NEWER);
    let amux = InstalledAmux::new();

    let stdout = succeeded(&amux.update(&server, &releases, &[]));

    assert!(
        stdout.contains(&format!(
            "the server `{}` runs amux {RELEASE} (protocol {}), which amux {NEWER} (protocol {}) \
             can't talk to; run `amux kill-server`",
            server.name(),
            compatible(),
            incompatible()
        )),
        "{stdout}"
    );
    assert!(server.is_listening());
}

#[test]
fn peers_still_on_this_release_are_named() {
    let [desk, laptop] = linked([TestServer::builder(), TestServer::builder()]);
    desk.wait_for_output(&["servers"], "laptop's version", |servers| {
        servers
            .lines()
            .any(|line| line.starts_with(laptop.name()) && line.contains(RELEASE))
    });
    let releases = FakeReleases::new();
    releases.publish(NEWER, &compatible()).set_latest(NEWER);
    let amux = InstalledAmux::new();

    let stdout = succeeded(&amux.update(&desk, &releases, &[]));

    assert!(
        stdout.contains(&format!(
            "`{}` runs amux {RELEASE} (protocol {}): run `amux update` there too",
            laptop.name(),
            compatible()
        )),
        "{stdout}"
    );
}
