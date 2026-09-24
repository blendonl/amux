mod detect;
pub mod git;
mod id;
mod registry;
#[cfg(test)]
mod testing;
mod worktree;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub use detect::{detect, Detected};
pub use id::ProjectId;
pub use registry::{Project, Registry};
pub use worktree::{
    check_removable, clone, default_branch, default_worktrees_dir, ensure_worktree, fetch_branch,
    remove_worktree,
};

fn canonical(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).with_context(|| format!("resolving {}", path.display()))
}
