use std::fmt;
use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::git;

const FILE_SCHEME: &str = "file://";
const SCHEME_SEPARATOR: &str = "://";
const GIT_SUFFIX: &str = ".git";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(String);

impl ProjectId {
    pub fn from_remote_url(url: &str) -> Self {
        match Remote::parse(url.trim()) {
            Remote::Network { host, path } => Self(network_id(host, path)),
            Remote::Local(path) => Self(local_id(&path)),
        }
    }

    pub fn from_root_commit(checkout: &Path) -> Result<Option<Self>> {
        let roots = git::run(checkout, ["rev-list", "--max-parents=0", "--branches"])?;
        Ok(roots.lines().min().map(|root| Self(root.to_owned())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
impl From<&str> for ProjectId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

pub(super) fn resolve_relative_local_url(url: &str, base: &Path) -> String {
    match Remote::parse(url.trim()) {
        Remote::Local(path) if path.is_relative() => normalize_path(&base.join(path))
            .to_string_lossy()
            .into_owned(),
        _ => url.to_owned(),
    }
}

enum Remote<'a> {
    Network { host: &'a str, path: &'a str },
    Local(PathBuf),
}

impl<'a> Remote<'a> {
    fn parse(url: &'a str) -> Self {
        if let Some(rest) = url.strip_prefix(FILE_SCHEME) {
            let path = rest.find('/').map_or("/", |start| &rest[start..]);
            return Self::Local(PathBuf::from(path));
        }
        if let Some((scheme, rest)) = url.split_once(SCHEME_SEPARATOR) {
            if is_scheme(scheme) {
                let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
                return Self::Network {
                    host: strip_port(strip_userinfo(authority)),
                    path,
                };
            }
        }
        match url.split_once(':') {
            Some((host, path)) if !host.contains('/') => Self::Network {
                host: strip_userinfo(host),
                path,
            },
            _ => Self::Local(PathBuf::from(url)),
        }
    }
}

fn is_scheme(scheme: &str) -> bool {
    !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn strip_userinfo(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

fn strip_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    }
}

fn network_id(host: &str, path: &str) -> String {
    let host = host.to_ascii_lowercase();
    let path = path.trim_matches('/');
    let path = path.strip_suffix(GIT_SUFFIX).unwrap_or(path);
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        host
    } else {
        format!("{host}/{path}")
    }
}

fn local_id(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    normalize_path(&absolute).to_string_lossy().into_owned()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::RootDir) => {}
                _ => normalized.push(Component::ParentDir),
            },
            other => normalized.push(other),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMUX: &str = "github.com/blendonl/amux";

    #[test]
    fn remote_urls_normalize_to_host_and_path() {
        let table = [
            ("git@github.com:blendonl/amux.git", AMUX),
            ("git@github.com:blendonl/amux", AMUX),
            ("github.com:blendonl/amux.git", AMUX),
            ("git@GitHub.com:blendonl/amux.git/", AMUX),
            ("https://github.com/blendonl/amux", AMUX),
            ("https://github.com/blendonl/amux.git", AMUX),
            ("https://github.com/blendonl/amux/", AMUX),
            ("https://github.com/blendonl/amux.git/", AMUX),
            ("https://GITHUB.COM/blendonl/amux", AMUX),
            ("https://user:token@github.com/blendonl/amux.git", AMUX),
            ("https://github.com:443/blendonl/amux.git", AMUX),
            ("http://github.com/blendonl/amux", AMUX),
            ("ssh://git@github.com/blendonl/amux", AMUX),
            ("ssh://git@github.com:22/blendonl/amux.git", AMUX),
            ("ssh://github.com/blendonl/amux.git", AMUX),
            ("git+ssh://git@github.com/blendonl/amux.git", AMUX),
            ("git://github.com/blendonl/amux.git", AMUX),
            ("  git://github.com/blendonl/amux.git\n", AMUX),
            (
                "git@gitlab.example.com:group/sub/Repo.git",
                "gitlab.example.com/group/sub/Repo",
            ),
            ("ssh://git@[::1]:2222/srv/amux.git", "[::1]/srv/amux"),
        ];
        for (url, expected) in table {
            assert_eq!(
                ProjectId::from_remote_url(url).as_str(),
                expected,
                "normalizing {url:?}"
            );
        }
    }

    #[test]
    fn local_origins_normalize_to_their_absolute_path() {
        let table = [
            ("/srv/git/amux.git", "/srv/git/amux.git"),
            ("/srv/git/amux/", "/srv/git/amux"),
            ("/srv/git/./other/../amux", "/srv/git/amux"),
            ("file:///srv/git/amux.git", "/srv/git/amux.git"),
            ("file:///srv/git/amux.git/", "/srv/git/amux.git"),
            ("file://localhost/srv/git/amux", "/srv/git/amux"),
        ];
        for (url, expected) in table {
            assert_eq!(
                ProjectId::from_remote_url(url).as_str(),
                expected,
                "normalizing {url:?}"
            );
        }
    }

    #[test]
    fn a_relative_local_path_is_not_mistaken_for_scp_syntax() {
        let id = ProjectId::from_remote_url("./repos/odd:name");
        assert!(Path::new(id.as_str()).is_absolute());
        assert!(id.as_str().ends_with("/repos/odd:name"));
    }

    #[test]
    fn relative_local_urls_resolve_against_a_base() {
        let base = Path::new("/home/tester/projects/amux");
        assert_eq!(
            resolve_relative_local_url("../origin.git", base),
            "/home/tester/projects/origin.git"
        );
        assert_eq!(
            resolve_relative_local_url("/srv/origin.git", base),
            "/srv/origin.git"
        );
        assert_eq!(
            resolve_relative_local_url("git@github.com:blendonl/amux.git", base),
            "git@github.com:blendonl/amux.git"
        );
    }

    #[test]
    fn the_id_serializes_as_a_plain_string() {
        #[derive(Serialize, Deserialize)]
        struct Wrapper {
            id: ProjectId,
        }
        let text = toml::to_string(&Wrapper {
            id: ProjectId::from_remote_url("git@github.com:blendonl/amux.git"),
        })
        .unwrap();
        assert_eq!(text.trim(), format!("id = \"{AMUX}\""));
        let parsed: Wrapper = toml::from_str(&text).unwrap();
        assert_eq!(parsed.id.as_str(), AMUX);
    }
}
