mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::releases::{target, FakeReleases};
use tempfile::TempDir;

const NEWER: &str = "99.0.0";
const PROTOCOL: &str = "9.0";
const SYSTEM_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

struct Machine {
    home: TempDir,
}

impl Machine {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("creating a home dir"),
        }
    }

    fn bin(&self) -> PathBuf {
        self.home.path().join(".local").join("bin")
    }

    fn install(&self, releases: &FakeReleases, env: &[(&str, &str)]) -> Output {
        Command::new("sh")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
            .env_clear()
            .env("HOME", self.home.path())
            .env("PATH", SYSTEM_PATH)
            .env("AMUX_RELEASES_URL", releases.url())
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()
            .expect("running install.sh")
    }

    fn version(&self, dir: &Path) -> String {
        let output = Command::new(dir.join("amux"))
            .arg("--version")
            .output()
            .expect("running the installed amux");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}

fn succeeded(output: &Output) -> String {
    assert!(
        output.status.success(),
        "install.sh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn failed(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "install.sh succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn installs_the_latest_release_in_local_bin() {
    let releases = FakeReleases::new();
    releases
        .publish("0.0.1", PROTOCOL)
        .publish(NEWER, PROTOCOL)
        .set_latest(NEWER);
    let machine = Machine::new();

    let stdout = succeeded(&machine.install(&releases, &[]));

    let bin = machine.bin();
    assert_eq!(
        machine.version(&bin),
        format!("amux {NEWER} (protocol {PROTOCOL})")
    );
    assert_eq!(files(&bin), ["amux"]);
    assert!(
        stdout.starts_with(&format!(
            "downloading amux {NEWER} for {}\ninstalled amux {NEWER} (protocol {PROTOCOL}) in {}\n",
            target(),
            bin.display()
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("{} is not on your PATH", bin.display())),
        "{stdout}"
    );
    assert!(stdout.contains("`amux update`"), "{stdout}");
}

#[test]
fn installs_the_version_and_directory_given() {
    let releases = FakeReleases::new();
    releases
        .publish("0.0.1", PROTOCOL)
        .publish(NEWER, PROTOCOL)
        .set_latest(NEWER);
    let machine = Machine::new();
    let dir = machine.home.path().join("tools");
    let path = format!("{}:{SYSTEM_PATH}", dir.display());

    for version in ["0.0.1", "v0.0.1"] {
        let stdout = succeeded(&machine.install(
            &releases,
            &[
                ("AMUX_VERSION", version),
                ("AMUX_INSTALL_DIR", &dir.display().to_string()),
                ("PATH", &path),
            ],
        ));
        assert_eq!(
            machine.version(&dir),
            format!("amux 0.0.1 (protocol {PROTOCOL})")
        );
        assert!(!stdout.contains("not on your PATH"), "{stdout}");
    }
    assert_eq!(files(&dir), ["amux"]);
}

#[test]
fn a_download_with_the_wrong_checksum_installs_nothing() {
    let releases = FakeReleases::new();
    releases
        .publish(NEWER, PROTOCOL)
        .set_latest(NEWER)
        .corrupt(NEWER);
    let machine = Machine::new();

    let stderr = failed(&machine.install(&releases, &[]));

    assert!(
        stderr.contains("does not match its checksum, so nothing was installed"),
        "{stderr}"
    );
    assert_eq!(files(&machine.bin()), Vec::<String>::new());
}

#[test]
fn a_release_that_is_not_published_installs_nothing() {
    let releases = FakeReleases::new();
    let machine = Machine::new();

    let stderr = failed(&machine.install(&releases, &[("AMUX_VERSION", "5.5.5")]));

    assert!(stderr.contains("could not download file://"), "{stderr}");
    assert!(stderr.contains("/download/v5.5.5/"), "{stderr}");
    assert_eq!(files(&machine.bin()), Vec::<String>::new());
}

#[test]
fn no_latest_release_is_an_error() {
    let releases = FakeReleases::new();
    let machine = Machine::new();

    let stderr = failed(&machine.install(&releases, &[]));

    assert!(
        stderr.contains("could not find the latest amux release"),
        "{stderr}"
    );
}

#[test]
fn a_download_that_does_not_run_installs_nothing() {
    let releases = FakeReleases::new();
    releases
        .publish_binary(NEWER, "#!/bin/sh\nexit 126\n")
        .set_latest(NEWER);
    let machine = Machine::new();

    let stderr = failed(&machine.install(&releases, &[]));

    assert!(
        stderr.contains(&format!(
            "the amux {NEWER} download does not run on this machine"
        )),
        "{stderr}"
    );
    assert_eq!(files(&machine.bin()), Vec::<String>::new());
}
