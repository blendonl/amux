use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::detect::detect;
use super::id::ProjectId;

const REGISTRY_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub url: Option<String>,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    path: PathBuf,
    projects: Vec<Project>,
}

#[derive(Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    projects: Vec<Project>,
}

impl Registry {
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let file: RegistryFile =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            projects: file.projects,
        })
    }

    pub fn add(&mut self, checkout: &Path) -> Result<Project> {
        let detected = detect(checkout)?.with_context(|| {
            format!(
                "{} is not a git repository with an origin remote or a commit",
                checkout.display()
            )
        })?;
        let project = Project {
            id: detected.id,
            name: detected.name,
            url: detected.url,
            path: detected.checkout,
        };
        match self
            .projects
            .iter_mut()
            .find(|known| known.id == project.id)
        {
            Some(known) => *known = project.clone(),
            None => self.projects.push(project.clone()),
        }
        Ok(project)
    }

    pub fn get(&self, id: &ProjectId) -> Option<&Project> {
        self.projects.iter().find(|project| &project.id == id)
    }

    pub fn find_by_name(&self, name: &str) -> Vec<&Project> {
        self.projects
            .iter()
            .filter(|project| project.name == name)
            .collect()
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    pub fn save(&self) -> Result<()> {
        let text = toml::to_string(&RegistryFile {
            projects: self.projects.clone(),
        })
        .context("serializing the project registry")?;
        let temp = self.temp_path()?;
        let written = write_private(&temp, &text).and_then(|()| fs::rename(&temp, &self.path));
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written.with_context(|| format!("writing {}", self.path.display()))
    }

    fn temp_path(&self) -> Result<PathBuf> {
        let name = self
            .path
            .file_name()
            .with_context(|| format!("{} has no file name", self.path.display()))?;
        let mut temp_name = name.to_owned();
        temp_name.push(format!(".{}.tmp", process::id()));
        Ok(self.path.with_file_name(temp_name))
    }
}

fn write_private(path: &Path, text: &str) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(REGISTRY_FILE_MODE)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::super::testing::{git, path_str, Fixture};
    use super::*;

    const REGISTRY: &str = "projects.toml";

    #[test]
    fn a_missing_file_is_an_empty_registry() {
        let fixture = Fixture::new();
        let registry = Registry::load(&fixture.path(REGISTRY)).unwrap();
        assert!(registry.projects().is_empty());
    }

    #[test]
    fn projects_round_trip_through_the_file() {
        let fixture = Fixture::new();
        let origin = fixture.origin();
        let amux = fixture.clone_of(&origin, "amux");
        let notes = fixture.repo("notes");
        let path = fixture.path(REGISTRY);

        let mut registry = Registry::load(&path).unwrap();
        let added = registry.add(&amux).unwrap();
        registry.add(&notes).unwrap();
        registry.save().unwrap();

        assert_eq!(
            added,
            Project {
                id: ProjectId::from_remote_url(path_str(&origin)),
                name: "amux".into(),
                url: Some(path_str(&origin).into()),
                path: amux,
            }
        );
        let loaded = Registry::load(&path).unwrap();
        assert_eq!(loaded, registry);
        assert_eq!(loaded.projects().len(), 2);
        assert_eq!(loaded.get(&added.id), Some(&added));
        assert_eq!(loaded.find_by_name("amux"), vec![&added]);
        assert_eq!(loaded.find_by_name("notes")[0].url, None);
        assert!(loaded.find_by_name("missing").is_empty());

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, REGISTRY_FILE_MODE);
        let leftovers: Vec<_> = fs::read_dir(fixture.path(""))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn adding_again_upserts_by_id() {
        let fixture = Fixture::new();
        let origin = fixture.origin();
        let first = fixture.clone_of(&origin, "first/amux");
        let second = fixture.clone_of(&origin, "second/amux");
        let linked = fixture.path("second/amux-worktrees/feature");
        git(
            &second,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature",
                path_str(&linked),
            ],
        );

        let mut registry = Registry::load(&fixture.path(REGISTRY)).unwrap();
        registry.add(&first).unwrap();
        let moved = registry.add(&linked).unwrap();

        assert_eq!(registry.projects(), [moved.clone()]);
        assert_eq!(moved.path, second);
    }

    #[test]
    fn adding_a_plain_directory_fails() {
        let fixture = Fixture::new();
        let plain = fixture.path("plain");
        fs::create_dir_all(&plain).unwrap();

        let mut registry = Registry::load(&fixture.path(REGISTRY)).unwrap();
        assert!(registry.add(&plain).is_err());
        assert!(registry.projects().is_empty());
    }

    #[test]
    fn a_corrupt_file_is_an_error() {
        let fixture = Fixture::new();
        let path = fixture.path(REGISTRY);
        fs::write(&path, "projects = 3").unwrap();
        assert!(Registry::load(&path).is_err());
    }
}
