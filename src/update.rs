use std::cmp::Ordering;
use std::env;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use semver::Version as Release;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::cli::UpdateArgs;
use crate::client::{self, Endpoint};
use crate::paths;
use crate::protocol::{self, IncompatibleServer, ServerStatus, ServerView, Version, Welcome};

const GITHUB_RELEASES: &str = "https://github.com/blendonl/amux/releases";
const GITHUB_API_RELEASES: &str = "https://api.github.com/repos/blendonl/amux/releases";
const RELEASES_ENV: &str = "AMUX_RELEASES_URL";
const GITHUB_JSON: &str = "Accept: application/vnd.github+json";
const BINARY: &str = "amux";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Releases {
    latest: String,
    download: String,
}

impl Releases {
    pub fn from_env() -> Self {
        match env::var(RELEASES_ENV) {
            Ok(url) if !url.is_empty() => Self::mirror(&url),
            _ => Self::github(),
        }
    }

    fn github() -> Self {
        Self {
            latest: format!("{GITHUB_API_RELEASES}/latest"),
            download: format!("{GITHUB_RELEASES}/download"),
        }
    }

    fn mirror(url: &str) -> Self {
        let url = url.trim_end_matches('/');
        Self {
            latest: format!("{url}/latest"),
            download: format!("{url}/download"),
        }
    }

    fn latest(&self) -> Result<Release> {
        #[derive(Deserialize)]
        struct Latest {
            tag_name: String,
        }

        let body =
            fetch(&self.latest, &[GITHUB_JSON]).context("finding the latest amux release")?;
        let latest: Latest = serde_json::from_slice(&body)
            .with_context(|| format!("reading the latest release from {}", self.latest))?;
        parse_release(&latest.tag_name)
    }

    fn asset(&self, release: &Release, name: &str) -> String {
        format!("{}/{}/{name}", self.download, tag(release))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Install,
    Installed,
    Ahead,
}

fn decide(current: &Release, wanted: &Release, pinned: bool) -> Decision {
    match current.cmp(wanted) {
        Ordering::Equal => Decision::Installed,
        Ordering::Greater if !pinned => Decision::Ahead,
        _ => Decision::Install,
    }
}

pub async fn run(endpoint: &Endpoint, args: UpdateArgs) -> Result<()> {
    let releases = Releases::from_env();
    let current = parse_release(protocol::RELEASE)?;
    let pinned = args.release.is_some();
    let wanted = match &args.release {
        Some(release) => parse_release(release)?,
        None => releases.latest()?,
    };

    match decide(&current, &wanted, pinned) {
        Decision::Installed if pinned => println!("this is already amux {current}"),
        Decision::Installed => println!("amux {current} is the latest release"),
        Decision::Ahead => {
            println!("this amux {current} is newer than the latest release, {wanted}")
        }
        Decision::Install if args.check => {
            println!(
                "amux {wanted} is out, this is amux {current}; run `amux update` to install it"
            )
        }
        Decision::Install => {
            let installed = install(&releases, &wanted)?;
            println!(
                "updated {} from amux {current} to amux {wanted}",
                installed.path.display()
            );
            if let Some(types) = refresh_lua_types(&installed.path, endpoint.config.as_deref()) {
                println!("updated the Lua types in {}", types.display());
            }
            if let Some((server, cluster)) = running_server(&endpoint.socket).await {
                print!("{}", after_update(&installed.version, &server, &cluster));
            }
        }
    }
    Ok(())
}

struct Installed {
    path: PathBuf,
    version: Version,
}

fn refresh_lua_types(binary: &Path, config: Option<&Path>) -> Option<PathBuf> {
    let types = paths::lua_types_file()
        .ok()
        .filter(|types| types.exists())?;
    let mut command = Command::new(binary);
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    command
        .args(["config", "lsp"])
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|output| output.status.success())
        .then_some(types)
}

fn install(releases: &Releases, release: &Release) -> Result<Installed> {
    let path = installed_binary()?;
    let target = host_target()?;
    println!("downloading amux {release} for {target}");
    let staging = TempDir::with_prefix("amux-update-").context("making a download directory")?;
    let binary = download(releases, release, &target, staging.path())?;
    let version = probe(&binary, release)?;
    replace(&path, &binary)?;
    Ok(Installed { path, version })
}

pub fn host_target() -> Result<String> {
    target_for(env::consts::OS, env::consts::ARCH)
}

fn target_for(os: &str, arch: &str) -> Result<String> {
    if !matches!(arch, "x86_64" | "aarch64") {
        bail!("there are no amux releases for {arch} machines; build amux from source");
    }
    match os {
        "linux" => Ok(format!("{arch}-unknown-linux-musl")),
        "macos" => Ok(format!("{arch}-apple-darwin")),
        other => bail!("there are no amux releases for {other}; build amux from source"),
    }
}

pub fn package(target: &str) -> String {
    format!("{BINARY}-{target}")
}

pub fn tag(release: &Release) -> String {
    format!("v{release}")
}

fn parse_release(text: &str) -> Result<Release> {
    let bare = text.trim();
    let bare = bare.strip_prefix('v').unwrap_or(bare);
    bare.parse()
        .with_context(|| format!("`{text}` is not a release version like 0.2.0"))
}

fn installed_binary() -> Result<PathBuf> {
    let exe = env::current_exe().context("finding this amux binary")?;
    fs::canonicalize(&exe).with_context(|| format!("resolving {}", exe.display()))
}

fn download(releases: &Releases, release: &Release, target: &str, dir: &Path) -> Result<PathBuf> {
    let package = package(target);
    let archive_name = format!("{package}.tar.gz");
    let archive = fetch(&releases.asset(release, &archive_name), &[])?;
    let checksum = fetch(
        &releases.asset(release, &format!("{archive_name}.sha256")),
        &[],
    )?;
    verify(&archive_name, &archive, &String::from_utf8_lossy(&checksum))?;

    let archive_path = dir.join(&archive_name);
    fs::write(&archive_path, &archive)
        .with_context(|| format!("writing {}", archive_path.display()))?;
    let output = run_tool(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(dir),
        "tar",
    )?;
    if !output.status.success() {
        bail!("unpacking {archive_name}: {}", stderr(&output));
    }

    let binary = dir.join(&package).join(BINARY);
    if !binary.is_file() {
        bail!("{archive_name} has no {package}/{BINARY}");
    }
    Ok(binary)
}

fn fetch(url: &str, headers: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("curl");
    command.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto-redir",
        "=https",
    ]);
    for header in headers {
        command.arg("--header").arg(header);
    }
    let output = run_tool(command.arg(url), "curl")?;
    if !output.status.success() {
        bail!("downloading {url}: {}", stderr(&output));
    }
    Ok(output.stdout)
}

fn run_tool(command: &mut Command, program: &str) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .output()
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                anyhow!("amux update needs {program}, and there is no {program} on PATH")
            }
            _ => anyhow!(err).context(format!("running {program}")),
        })
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_owned()
}

fn verify(name: &str, archive: &[u8], checksum_file: &str) -> Result<()> {
    let expected = checksum_file
        .split_whitespace()
        .next()
        .with_context(|| format!("{name}.sha256 is empty"))?
        .to_ascii_lowercase();
    let actual = protocol::hex(&Sha256::digest(archive));
    if expected != actual {
        bail!(
            "{name} does not match its checksum, so nothing was installed: \
             {name}.sha256 says {expected}, the download hashes to {actual}"
        );
    }
    Ok(())
}

fn probe(binary: &Path, release: &Release) -> Result<Version> {
    let output = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("the amux {release} download does not run on this machine"))?;
    if !output.status.success() {
        bail!(
            "the amux {release} download does not run on this machine: {}",
            stderr(&output)
        );
    }
    let version: Version = String::from_utf8_lossy(&output.stdout).parse()?;
    if version.release != release.to_string() {
        bail!("the download for amux {release} is {version}");
    }
    Ok(version)
}

fn replace(path: &Path, binary: &Path) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("{} is not in a directory", path.display()))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".amux-update-")
        .tempfile_in(dir)
        .with_context(|| {
            format!(
                "writing to {}; run amux update as a user who can write there",
                dir.display()
            )
        })?;
    io::copy(
        &mut File::open(binary).with_context(|| format!("opening {}", binary.display()))?,
        staged.as_file_mut(),
    )
    .with_context(|| format!("copying amux into {}", dir.display()))?;
    let permissions = fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .permissions();
    staged.as_file().set_permissions(permissions)?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .map_err(|err| err.error)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

async fn running_server(socket: &Path) -> Option<(Welcome, Vec<ServerView>)> {
    match client::greet_and_list_cluster(socket).await {
        Ok(found) => Some(found),
        Err(err) => err
            .downcast::<IncompatibleServer>()
            .ok()?
            .server
            .map(|welcome| (welcome, Vec::new())),
    }
}

fn after_update(installed: &Version, server: &Welcome, cluster: &[ServerView]) -> String {
    let mut out = String::new();
    let running = &server.version;
    let name = &server.server_name;
    let release = &installed.release;
    if running.release != *release {
        if installed.is_compatible_with(running.major) {
            out.push_str(&format!(
                "the server `{name}` still runs {running} and keeps its sessions, and amux \
                 {release} can talk to it; `amux kill-server` stops it (this ends its sessions) \
                 and the next amux command starts amux {release}\n"
            ));
        } else {
            out.push_str(&format!(
                "the server `{name}` runs {running}, which {installed} can't talk to; \
                 run `amux kill-server` to stop it (this ends its sessions) before you use \
                 amux again\n"
            ));
        }
    }

    for peer in cluster {
        let version = match &peer.status {
            ServerStatus::Local => continue,
            ServerStatus::Incompatible { version } => Some(version),
            _ => peer.version.as_ref(),
        };
        let Some(version) = version.filter(|version| version.release != *release) else {
            continue;
        };
        let reach = if installed.is_compatible_with(version.major) {
            ""
        } else {
            ", which can't link to this one"
        };
        out.push_str(&format!(
            "`{}` runs {version}{reach}: run `amux update` there too\n",
            peer.name
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::*;

    fn release(text: &str) -> Release {
        parse_release(text).unwrap()
    }

    fn version(release: &str, major: u16) -> Version {
        Version {
            release: release.to_owned(),
            major,
            minor: 0,
        }
    }

    fn welcome(release: &str, major: u16) -> Welcome {
        Welcome {
            server_name: "desk".into(),
            version: version(release, major),
        }
    }

    fn peer(name: &str, status: ServerStatus, version: Option<Version>) -> ServerView {
        ServerView {
            id: None,
            name: name.into(),
            address: None,
            version,
            status,
            sessions: Vec::new(),
            projects: Vec::new(),
        }
    }

    #[test]
    fn releases_read_with_or_without_the_v() {
        assert_eq!(release("v0.2.0"), release("0.2.0"));
        assert_eq!(release(" 1.10.3\n").to_string(), "1.10.3");
        assert_eq!(tag(&release("0.2.0")), "v0.2.0");
        let err = parse_release("latest").unwrap_err().to_string();
        assert!(err.contains("`latest` is not a release version"), "{err}");
    }

    #[test]
    fn only_an_older_build_updates_to_the_latest() {
        let old = release("0.1.0");
        let new = release("0.2.0");
        assert_eq!(decide(&old, &new, false), Decision::Install);
        assert_eq!(decide(&new, &new, false), Decision::Installed);
        assert_eq!(decide(&new, &old, false), Decision::Ahead);
        assert_eq!(
            decide(&release("0.2.0-rc.1"), &new, false),
            Decision::Install
        );
    }

    #[test]
    fn a_pinned_release_installs_even_when_older() {
        let old = release("0.1.0");
        let new = release("0.2.0");
        assert_eq!(decide(&new, &old, true), Decision::Install);
        assert_eq!(decide(&new, &new, true), Decision::Installed);
    }

    #[test]
    fn github_and_mirrors_share_one_layout() {
        let github = Releases::github();
        assert_eq!(
            github.latest,
            "https://api.github.com/repos/blendonl/amux/releases/latest"
        );
        assert_eq!(
            github.asset(&release("0.2.0"), "amux-x86_64-apple-darwin.tar.gz"),
            "https://github.com/blendonl/amux/releases/download/v0.2.0/amux-x86_64-apple-darwin.tar.gz"
        );

        let mirror = Releases::mirror("file:///srv/amux/");
        assert_eq!(mirror.latest, "file:///srv/amux/latest");
        assert_eq!(
            mirror.asset(&release("1.0.0"), "a.sha256"),
            "file:///srv/amux/download/v1.0.0/a.sha256"
        );
    }

    #[test]
    fn targets_follow_the_release_matrix() {
        assert_eq!(
            target_for("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            target_for("linux", "aarch64").unwrap(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            target_for("macos", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            target_for("macos", "x86_64").unwrap(),
            "x86_64-apple-darwin"
        );
        assert!(target_for("freebsd", "x86_64").is_err());
        assert!(target_for("linux", "riscv64").is_err());
        assert_eq!(package("aarch64-apple-darwin"), "amux-aarch64-apple-darwin");
    }

    #[test]
    fn a_checksum_file_must_match_the_archive() {
        let archive = b"not really a tarball";
        let hash = protocol::hex(&Sha256::digest(archive));
        verify("a.tar.gz", archive, &format!("{hash}  a.tar.gz\n")).unwrap();
        verify("a.tar.gz", archive, &hash.to_ascii_uppercase()).unwrap();

        let err = verify("a.tar.gz", b"tampered", &format!("{hash}  a.tar.gz"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("nothing was installed"), "{err}");
        assert!(err.contains(&hash), "{err}");

        let err = verify("a.tar.gz", archive, " \n").unwrap_err().to_string();
        assert!(err.contains("a.tar.gz.sha256 is empty"), "{err}");
    }

    #[test]
    fn replacing_keeps_the_mode_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("amux");
        fs::write(&installed, "old").unwrap();
        fs::set_permissions(&installed, fs::Permissions::from_mode(0o750)).unwrap();
        let download = dir.path().join("download");
        fs::write(&download, "new").unwrap();

        replace(&installed, &download).unwrap();

        assert_eq!(fs::read_to_string(&installed).unwrap(), "new");
        let mode = fs::metadata(&installed).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o750);
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["amux", "download"]);
    }

    #[test]
    fn a_compatible_server_keeps_running_until_you_restart_it() {
        let note = after_update(&version("0.2.0", 9), &welcome("0.1.0", 9), &[]);
        assert!(
            note.contains("the server `desk` still runs amux 0.1.0 (protocol 9.0)"),
            "{note}"
        );
        assert!(note.contains("can talk to it"), "{note}");
        assert!(note.contains("`amux kill-server`"), "{note}");
    }

    #[test]
    fn an_incompatible_server_has_to_stop() {
        let note = after_update(&version("0.2.0", 10), &welcome("0.1.0", 9), &[]);
        assert!(
            note.contains("which amux 0.2.0 (protocol 10.0) can't talk to"),
            "{note}"
        );
        assert!(note.contains("run `amux kill-server`"), "{note}");
    }

    #[test]
    fn a_server_already_on_the_release_needs_nothing() {
        let note = after_update(&version("0.2.0", 9), &welcome("0.2.0", 9), &[]);
        assert_eq!(note, "");
    }

    #[test]
    fn peers_on_other_releases_are_named() {
        let online = ServerStatus::Online {
            latency: Some(Duration::from_millis(3)),
        };
        let cluster = [
            peer("desk", ServerStatus::Local, Some(version("0.1.0", 9))),
            peer("laptop", online.clone(), Some(version("0.1.0", 9))),
            peer("nas", online, Some(version("0.2.0", 9))),
            peer(
                "pi",
                ServerStatus::Incompatible {
                    version: version("0.0.9", 8),
                },
                None,
            ),
            peer(
                "gone",
                ServerStatus::Offline {
                    last_seen: None,
                    stopped: false,
                },
                None,
            ),
        ];
        let note = after_update(&version("0.2.0", 9), &welcome("0.2.0", 9), &cluster);
        assert_eq!(
            note,
            "`laptop` runs amux 0.1.0 (protocol 9.0): run `amux update` there too\n\
             `pi` runs amux 0.0.9 (protocol 8.0), which can't link to this one: \
             run `amux update` there too\n"
        );
    }
}
