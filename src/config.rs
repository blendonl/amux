use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use toml_edit::{DocumentMut, Item, Table};

use crate::paths;

const DEFAULT_PROJECTS_DIR: &str = "projects";
const SERVERS_TABLE: &str = "servers";
const SERVER_ID_FILE: &str = "server-id";
const SERVER_ID_FILE_MODE: u32 = 0o600;
const SERVER_ID_HEX_DIGITS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub name: String,
    pub projects_dir: PathBuf,
    pub servers: BTreeMap<String, ServerConfig>,
    pub projects: BTreeMap<String, ProjectConfig>,
    pub discovery: DiscoveryConfig,
    pub lan: LanConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub address: String,
    pub amux_path: Option<String>,
    pub socket: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub default_server: Option<String>,
    pub worktrees_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DiscoveryConfig {
    pub tailscale: bool,
    pub lan: bool,
    pub tailscale_tags: Vec<String>,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            tailscale: true,
            lan: true,
            tailscale_tags: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LanConfig {
    pub port: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    name: Option<String>,
    projects_dir: Option<PathBuf>,
    #[serde(default)]
    servers: BTreeMap<String, ServerConfig>,
    #[serde(default)]
    projects: BTreeMap<String, ProjectConfig>,
    #[serde(default)]
    discovery: DiscoveryConfig,
    #[serde(default)]
    lan: LanConfig,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = read_optional(path)?.unwrap_or_default();
        Self::parse(&text, &paths::home_dir()?, &hostname()?)
            .with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str, home: &Path, hostname: &str) -> Result<Self> {
        let file: ConfigFile = toml::from_str(text)?;
        let name = file.name.unwrap_or_else(|| hostname.to_owned());
        if name.trim().is_empty() {
            bail!("the server name must not be empty");
        }
        let projects_dir = file
            .projects_dir
            .unwrap_or_else(|| PathBuf::from("~").join(DEFAULT_PROJECTS_DIR));
        let projects = file
            .projects
            .into_iter()
            .map(|(project, config)| {
                let worktrees_dir = config.worktrees_dir.map(|dir| expand_home(dir, home));
                (
                    project,
                    ProjectConfig {
                        worktrees_dir,
                        ..config
                    },
                )
            })
            .collect();

        Ok(Self {
            name,
            projects_dir: expand_home(projects_dir, home),
            servers: file.servers,
            projects,
            discovery: file.discovery,
            lan: file.lan,
        })
    }
}

pub fn add_server(path: &Path, name: &str, server: &ServerConfig) -> Result<()> {
    edit(path, |document| {
        let servers = document
            .entry(SERVERS_TABLE)
            .or_insert_with(implicit_table)
            .as_table_mut()
            .context("`servers` is not a table")?;
        if servers.contains_key(name) {
            bail!("{name} is already in the config");
        }
        let mut entry = Table::new();
        entry.insert("address", toml_edit::value(server.address.as_str()));
        if let Some(amux_path) = &server.amux_path {
            entry.insert("amux_path", toml_edit::value(amux_path.as_str()));
        }
        if let Some(socket) = &server.socket {
            entry.insert("socket", toml_edit::value(socket.as_str()));
        }
        servers.insert(name, Item::Table(entry));
        Ok(())
    })
}

pub fn remove_server(path: &Path, name: &str) -> Result<()> {
    edit(path, |document| {
        document
            .get_mut(SERVERS_TABLE)
            .and_then(Item::as_table_like_mut)
            .and_then(|servers| servers.remove(name))
            .with_context(|| format!("no server named {name} in the config"))?;
        Ok(())
    })
}

fn edit(path: &Path, change: impl FnOnce(&mut DocumentMut) -> Result<()>) -> Result<()> {
    let text = read_optional(path)?.unwrap_or_default();
    let mut document: DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    change(&mut document).with_context(|| format!("editing {}", path.display()))?;
    let edited = document.to_string();
    Config::parse(&edited, &paths::home_dir()?, &hostname()?)
        .with_context(|| format!("the edited {} would not load", path.display()))?;

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, edited).with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
}

fn implicit_table() -> Item {
    let mut table = Table::new();
    table.set_implicit(true);
    Item::Table(table)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    pub id: ServerId,
    pub name: String,
    pub incarnation: Incarnation,
}

impl ServerIdentity {
    pub fn load(state_dir: &Path, name: String) -> Result<Self> {
        Ok(Self {
            id: ServerId::load_or_create(&state_dir.join(SERVER_ID_FILE))?,
            name,
            incarnation: Incarnation::random()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServerId(u128);

impl ServerId {
    pub fn random() -> Result<Self> {
        Ok(Self(u128::from_ne_bytes(random_bytes()?)))
    }

    fn load_or_create(path: &Path) -> Result<Self> {
        if let Some(id) = Self::read(path)? {
            return Ok(id);
        }
        let id = Self::random()?;
        let created = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(SERVER_ID_FILE_MODE)
            .open(path);
        match created {
            Ok(mut file) => {
                writeln!(file, "{id}").with_context(|| format!("writing {}", path.display()))?;
                Ok(id)
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Self::read(path)?
                .with_context(|| format!("{} disappeared while reading it", path.display())),
            Err(err) => Err(err).with_context(|| format!("creating {}", path.display())),
        }
    }

    fn read(path: &Path) -> Result<Option<Self>> {
        let Some(text) = read_optional(path)? else {
            return Ok(None);
        };
        let id = text
            .trim()
            .parse()
            .with_context(|| format!("reading the server id from {}", path.display()))?;
        Ok(Some(id))
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl FromStr for ServerId {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        if text.len() != SERVER_ID_HEX_DIGITS || !text.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("{text:?} is not a {SERVER_ID_HEX_DIGITS} digit hex server id");
        }
        Ok(Self(u128::from_str_radix(text, 16)?))
    }
}

impl Serialize for ServerId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.collect_str(self)
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for ServerId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let text = String::deserialize(deserializer)?;
            text.parse().map_err(D::Error::custom)
        } else {
            u128::deserialize(deserializer).map(Self)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Incarnation(u64);

impl Incarnation {
    pub fn random() -> Result<Self> {
        Ok(Self(u64::from_ne_bytes(random_bytes()?)))
    }
}

impl fmt::Display for Incarnation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

fn expand_home(path: PathBuf, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) if rest.as_os_str().is_empty() => home.to_owned(),
        Ok(rest) => home.join(rest),
        Err(_) => path,
    }
}

fn hostname() -> Result<String> {
    let name = nix::unistd::gethostname().context("reading the hostname")?;
    Ok(name.to_string_lossy().into_owned())
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|err| anyhow!("reading random bytes: {err}"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const HOME: &str = "/home/tester";
    const HOSTNAME: &str = "box";

    fn parse(text: &str) -> Result<Config> {
        Config::parse(text, Path::new(HOME), HOSTNAME)
    }

    #[test]
    fn an_empty_file_gives_the_defaults() {
        assert_eq!(
            parse("").unwrap(),
            Config {
                name: HOSTNAME.into(),
                projects_dir: PathBuf::from("/home/tester/projects"),
                servers: BTreeMap::new(),
                projects: BTreeMap::new(),
                discovery: DiscoveryConfig {
                    tailscale: true,
                    lan: true,
                    tailscale_tags: Vec::new(),
                },
                lan: LanConfig { port: 0 },
            }
        );
    }

    #[test]
    fn discovery_sources_can_be_turned_off_and_tuned() {
        let config = parse(
            r#"
            [discovery]
            tailscale = false
            tailscale_tags = ["tag:amux"]

            [lan]
            port = 7448
            "#,
        )
        .unwrap();

        assert_eq!(
            config.discovery,
            DiscoveryConfig {
                tailscale: false,
                lan: true,
                tailscale_tags: vec!["tag:amux".into()],
            }
        );
        assert_eq!(config.lan, LanConfig { port: 7448 });
    }

    #[test]
    fn a_missing_file_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_optional(&dir.path().join("config.toml")).unwrap(),
            None
        );
    }

    #[test]
    fn the_documented_example_parses() {
        let config = parse(
            r#"
            name = "desktop"
            projects_dir = "~/code"

            [servers.laptop]
            address = "ssh://laptop"

            [servers.home-server]
            address = "ssh://notpc@home-server"
            amux_path = "~/.cargo/bin/amux"
            socket = "dev"

            [projects.amux]
            default_server = "desktop"
            worktrees_dir = "~/projects/amux-worktrees"

            [projects.notes]
            "#,
        )
        .unwrap();

        assert_eq!(config.name, "desktop");
        assert_eq!(config.projects_dir, PathBuf::from("/home/tester/code"));
        assert_eq!(
            config.servers["laptop"],
            ServerConfig {
                address: "ssh://laptop".into(),
                amux_path: None,
                socket: None,
            }
        );
        assert_eq!(
            config.servers["home-server"],
            ServerConfig {
                address: "ssh://notpc@home-server".into(),
                amux_path: Some("~/.cargo/bin/amux".into()),
                socket: Some("dev".into()),
            }
        );
        assert_eq!(
            config.projects["amux"],
            ProjectConfig {
                default_server: Some("desktop".into()),
                worktrees_dir: Some(PathBuf::from("/home/tester/projects/amux-worktrees")),
            }
        );
        assert_eq!(config.projects["notes"], ProjectConfig::default());
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        assert!(parse("prefix = \"C-a\"").is_err());
        assert!(parse("[servers.laptop]\naddress = \"ssh://laptop\"\nport = 22").is_err());
        assert!(parse("[projects.amux]\nbranch = \"main\"").is_err());
        assert!(parse("[discovery]\nmdns = true").is_err());
        assert!(parse("[lan]\naddress = \"0.0.0.0\"").is_err());
    }

    #[test]
    fn a_server_needs_an_address() {
        assert!(parse("[servers.laptop]\namux_path = \"amux\"").is_err());
    }

    #[test]
    fn an_empty_name_is_rejected() {
        assert!(parse("name = \" \"").is_err());
    }

    #[test]
    fn only_a_leading_tilde_is_expanded() {
        let home = Path::new(HOME);
        assert_eq!(expand_home("~".into(), home), PathBuf::from(HOME));
        assert_eq!(
            expand_home("~/a".into(), home),
            PathBuf::from("/home/tester/a")
        );
        assert_eq!(
            expand_home("~other/a".into(), home),
            PathBuf::from("~other/a")
        );
        assert_eq!(expand_home("/srv/~".into(), home), PathBuf::from("/srv/~"));
    }

    #[test]
    fn the_server_id_persists_and_the_incarnation_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let first = ServerIdentity::load(dir.path(), "a".into()).unwrap();
        let second = ServerIdentity::load(dir.path(), "a".into()).unwrap();

        assert_eq!(first.id, second.id);
        assert_ne!(first.incarnation, second.incarnation);

        let path = dir.path().join(SERVER_ID_FILE);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{}\n", first.id)
        );
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, SERVER_ID_FILE_MODE);
    }

    #[test]
    fn a_corrupt_server_id_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SERVER_ID_FILE), "not-an-id\n").unwrap();
        assert!(ServerIdentity::load(dir.path(), "a".into()).is_err());
    }

    fn laptop() -> ServerConfig {
        ServerConfig {
            address: "ssh://laptop".into(),
            amux_path: None,
            socket: None,
        }
    }

    #[test]
    fn adding_a_server_creates_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("amux").join("config.toml");
        let server = ServerConfig {
            amux_path: Some("~/.cargo/bin/amux".into()),
            socket: Some("dev".into()),
            ..laptop()
        };

        add_server(&path, "laptop", &server).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[servers.laptop]\naddress = \"ssh://laptop\"\namux_path = \"~/.cargo/bin/amux\"\nsocket = \"dev\"\n"
        );
    }

    #[test]
    fn adding_and_removing_servers_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original =
            "# my machines\nname = \"desk\"\n\n[projects.amux]\ndefault_server = \"desk\"\n";
        fs::write(&path, original).unwrap();

        add_server(&path, "laptop", &laptop()).unwrap();
        let added = fs::read_to_string(&path).unwrap();
        assert!(added.starts_with(original), "{added}");
        assert!(
            added.contains("[servers.laptop]\naddress = \"ssh://laptop\"\n"),
            "{added}"
        );

        remove_server(&path, "laptop").unwrap();
        let removed = fs::read_to_string(&path).unwrap();
        assert!(removed.starts_with(original), "{removed}");
        assert!(!removed.contains("laptop"), "{removed}");
    }

    #[test]
    fn adding_a_known_server_or_removing_an_unknown_one_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        add_server(&path, "laptop", &laptop()).unwrap();

        let duplicate = add_server(&path, "laptop", &laptop()).unwrap_err();
        assert!(format!("{duplicate:#}").contains("already in the config"));
        let missing = remove_server(&path, "desk").unwrap_err();
        assert!(format!("{missing:#}").contains("no server named desk"));
    }

    #[test]
    fn an_edit_that_breaks_the_config_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "servers = 3\n").unwrap();

        assert!(add_server(&path, "laptop", &laptop()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "servers = 3\n");
    }

    #[test]
    fn server_ids_are_a_number_on_the_wire_and_hex_in_text() {
        let id: ServerId = "0123456789abcdef0123456789abcdef".parse().unwrap();
        let wire = postcard::to_stdvec(&id).unwrap();
        assert_eq!(wire, postcard::to_stdvec(&id.0).unwrap());
        assert_eq!(postcard::from_bytes::<ServerId>(&wire).unwrap(), id);

        let text = serde_json::to_string(&id).unwrap();
        assert_eq!(text, "\"0123456789abcdef0123456789abcdef\"");
        assert_eq!(serde_json::from_str::<ServerId>(&text).unwrap(), id);
        assert!(serde_json::from_str::<ServerId>("\"nope\"").is_err());
    }

    #[test]
    fn server_ids_round_trip_through_hex() {
        let id = ServerId::random().unwrap();
        let text = id.to_string();
        assert_eq!(text.len(), SERVER_ID_HEX_DIGITS);
        assert_eq!(text.parse::<ServerId>().unwrap(), id);
        assert!("12345".parse::<ServerId>().is_err());
        assert!("z"
            .repeat(SERVER_ID_HEX_DIGITS)
            .parse::<ServerId>()
            .is_err());
    }
}
