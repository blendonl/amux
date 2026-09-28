use std::fmt::Write;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use sha2::{Digest, Sha256};
use tempfile::TempDir;

pub struct FakeReleases {
    root: TempDir,
}

impl FakeReleases {
    pub fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("creating the releases dir"),
        }
    }

    pub fn url(&self) -> String {
        format!("file://{}", self.root.path().display())
    }

    pub fn publish(&self, release: &str, protocol: &str) -> &Self {
        self.publish_binary(
            release,
            &format!("#!/bin/sh\necho 'amux {release} (protocol {protocol})'\n"),
        )
    }

    pub fn publish_binary(&self, release: &str, script: &str) -> &Self {
        let package = package();
        let staging = tempfile::tempdir().expect("creating a staging dir");
        let dir = staging.path().join(&package);
        fs::create_dir_all(&dir).expect("creating the package dir");
        let binary = dir.join("amux");
        fs::write(&binary, script).expect("writing the fake amux");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
            .expect("making the fake amux executable");

        let release_dir = self.release_dir(release);
        fs::create_dir_all(&release_dir).expect("creating the release dir");
        let archive = release_dir.join(archive_name());
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(staging.path())
            .arg(&package)
            .status()
            .expect("running tar");
        assert!(status.success(), "tar failed");

        let hash = sha256(&fs::read(&archive).expect("reading the archive"));
        self.write_checksum(release, &hash);
        self
    }

    pub fn set_latest(&self, release: &str) -> &Self {
        fs::write(
            self.root.path().join("latest"),
            format!("{{\n  \"tag_name\": \"v{release}\",\n  \"name\": \"amux {release}\"\n}}\n"),
        )
        .expect("writing latest");
        self
    }

    pub fn corrupt(&self, release: &str) -> &Self {
        self.write_checksum(release, &"0".repeat(64));
        self
    }

    fn write_checksum(&self, release: &str, hash: &str) {
        let name = archive_name();
        fs::write(
            self.release_dir(release).join(format!("{name}.sha256")),
            format!("{hash}  {name}\n"),
        )
        .expect("writing the checksum");
    }

    fn release_dir(&self, release: &str) -> PathBuf {
        self.root
            .path()
            .join("download")
            .join(format!("v{release}"))
    }
}

pub fn target() -> String {
    amux::update::host_target().expect("a release target for this machine")
}

fn package() -> String {
    amux::update::package(&target())
}

fn archive_name() -> String {
    format!("{}.tar.gz", package())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}
