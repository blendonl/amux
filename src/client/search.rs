use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::projects;
use crate::paths;
use crate::project;
use crate::protocol::ProjectRef;

const GIT_DIR: &str = ".git";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub label: String,
    pub detail: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Projects { dirs: Vec<PathBuf>, depth: u32 },
    Worktrees { checkout: PathBuf },
    Open(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Candidates(Result<Vec<Candidate>, String>),
    Opened(Result<Opening, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opening {
    pub cwd: PathBuf,
    pub project: Option<ProjectRef>,
    pub branch: Option<String>,
}

impl Job {
    pub fn run(self, home: Option<&Path>) -> Found {
        match self {
            Self::Projects { dirs, depth } => {
                Found::Candidates(Ok(scan_projects(&dirs, depth, home)))
            }
            Self::Worktrees { checkout } => {
                Found::Candidates(worktrees(&checkout, home).map_err(|err| format!("{err:#}")))
            }
            Self::Open(path) => Found::Opened(open(&path).map_err(|err| format!("{err:#}"))),
        }
    }
}

fn scan_projects(dirs: &[PathBuf], depth: u32, home: Option<&Path>) -> Vec<Candidate> {
    let mut scan = Scan {
        home,
        seen: HashSet::new(),
        found: Vec::new(),
    };
    for dir in dirs {
        let root = match home {
            Some(home) => paths::expand_home(dir.clone(), home),
            None => dir.clone(),
        };
        scan.walk(&root, &root, depth);
    }
    let mut found = scan.found;
    found.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.path.cmp(&b.path)));
    found
}

struct Scan<'a> {
    home: Option<&'a Path>,
    seen: HashSet<PathBuf>,
    found: Vec<Candidate>,
}

impl Scan<'_> {
    fn walk(&mut self, root: &Path, dir: &Path, depth: u32) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut children: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        children.sort();
        for child in children {
            if child.join(GIT_DIR).is_dir() {
                self.add(root, child);
            } else {
                self.walk(root, &child, depth - 1);
            }
        }
    }

    fn add(&mut self, root: &Path, repo: PathBuf) {
        let Ok(canonical) = fs::canonicalize(&repo) else {
            return;
        };
        if !self.seen.insert(canonical) {
            return;
        }
        let label = repo
            .strip_prefix(root)
            .unwrap_or(&repo)
            .display()
            .to_string();
        self.found.push(Candidate {
            label,
            detail: shown(&repo, self.home),
            path: repo,
        });
    }
}

fn worktrees(checkout: &Path, home: Option<&Path>) -> Result<Vec<Candidate>> {
    Ok(project::branch_worktrees(checkout)?
        .into_iter()
        .map(|worktree| Candidate {
            label: worktree.branch,
            detail: shown(&worktree.path, home),
            path: worktree.path,
        })
        .collect())
}

fn open(path: &Path) -> Result<Opening> {
    let detected = project::detect(path)?.filter(|detected| detected.branch.is_some());
    Ok(Opening {
        cwd: path.to_owned(),
        branch: detected
            .as_ref()
            .and_then(|detected| detected.branch.clone()),
        project: detected.as_ref().map(projects::from_detected),
    })
}

fn shown(path: &Path, home: Option<&Path>) -> String {
    match home {
        Some(home) => paths::abbreviate_home(path, home),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::project::testing::{git, path_str, Fixture};

    fn labels(found: &[Candidate]) -> Vec<&str> {
        found
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect()
    }

    fn make_repo_dir(path: &Path) {
        fs::create_dir_all(path.join(GIT_DIR)).unwrap();
    }

    #[test]
    fn repos_are_found_down_to_the_depth_and_nothing_else() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let projects = home.join("projects");
        make_repo_dir(&projects.join("amux"));
        make_repo_dir(&projects.join("work/api"));
        make_repo_dir(&projects.join("work/api/nested"));
        make_repo_dir(&projects.join("a/b/too-deep"));
        make_repo_dir(&projects.join(".hidden"));
        fs::create_dir_all(projects.join("amux-worktrees/feature")).unwrap();
        fs::write(projects.join("amux-worktrees/feature/.git"), "gitdir: x").unwrap();
        fs::write(projects.join("notes.txt"), "").unwrap();
        let dirs = vec![PathBuf::from("~/projects")];

        let shallow = scan_projects(&dirs, 1, Some(home));
        assert_eq!(labels(&shallow), ["amux"]);
        assert_eq!(shallow[0].detail, "~/projects/amux");
        assert_eq!(shallow[0].path, projects.join("amux"));

        let deeper = scan_projects(&dirs, 2, Some(home));
        assert_eq!(labels(&deeper), ["amux", "work/api"]);
        assert_eq!(deeper[1].detail, "~/projects/work/api");
    }

    #[test]
    fn every_dir_is_scanned_once_and_missing_ones_are_skipped() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        make_repo_dir(&home.join("projects/amux"));
        make_repo_dir(&home.join("Projects/notes"));
        fs::create_dir_all(home.join("code")).unwrap();
        symlink(home.join("projects/amux"), home.join("code/amux-link")).unwrap();
        let dirs: Vec<PathBuf> = [
            "~/projects",
            "~/Projects",
            "~/missing",
            "~/projects",
            "~/code",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();

        let found = scan_projects(&dirs, 1, Some(home));

        assert_eq!(labels(&found), ["amux", "notes"]);
        assert_eq!(found[1].detail, "~/Projects/notes");
    }

    #[test]
    fn without_a_home_the_paths_are_used_as_given() {
        let root = tempfile::tempdir().unwrap();
        make_repo_dir(&root.path().join("amux"));

        let found = scan_projects(&[root.path().to_owned()], 1, None);

        assert_eq!(
            found[0].detail,
            root.path().join("amux").display().to_string()
        );
        assert!(scan_projects(&[PathBuf::from("~/projects")], 1, None).is_empty());
    }

    #[test]
    fn a_checkouts_worktrees_become_candidates_by_branch() {
        let fixture = Fixture::new();
        let repo = fixture.repo("amux");
        let feature = fixture.path("amux-worktrees/feature-x");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature-x",
                path_str(&feature),
            ],
        );

        let found = worktrees(&repo, Some(&fixture.path(""))).unwrap();

        assert_eq!(labels(&found), ["main", "feature-x"]);
        assert_eq!(found[1].detail, "~/amux-worktrees/feature-x");
        assert_eq!(found[1].path, feature);
        assert!(worktrees(&fixture.path("nowhere"), None).is_err());
    }

    #[test]
    fn opening_a_repo_names_its_project_and_branch() {
        let fixture = Fixture::new();
        let repo = fixture.repo("amux");
        let plain = fixture.path("plain");
        fs::create_dir_all(&plain).unwrap();

        let opened = open(&repo).unwrap();
        assert_eq!(opened.cwd, repo);
        assert_eq!(opened.branch.as_deref(), Some("main"));
        assert_eq!(
            opened.project.map(|project| project.name).as_deref(),
            Some("amux")
        );

        git(&repo, &["switch", "--quiet", "--detach"]);
        assert_eq!(
            open(&repo).unwrap(),
            Opening {
                cwd: repo.clone(),
                project: None,
                branch: None,
            }
        );
        assert_eq!(
            open(&plain).unwrap(),
            Opening {
                cwd: plain,
                project: None,
                branch: None,
            }
        );
    }
}
