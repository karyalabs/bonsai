//! Finding, opening and creating repositories.

use std::path::{Path, PathBuf};

use crate::{Error, Git, Result};

/// A local Git repository Bonsai has opened (CONTEXT.md: Repository).
#[derive(Clone, Debug)]
pub struct Repository {
    git: Git,
    git_dir: PathBuf,
    work_dir: Option<PathBuf>,
}

impl Repository {
    /// Finds the repository containing `path`, searching parent folders like
    /// `git` does.
    pub fn discover(git: &Git, path: &Path) -> Result<Self> {
        let output = git.run_unchecked(
            &git.command([
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--is-bare-repository",
            ])
            .cwd(path),
        )?;
        if !output.status.success() {
            return Err(Error::NotARepository(path.to_path_buf()));
        }
        let stdout = output.stdout_lossy();
        let mut lines = stdout.lines();
        let git_dir = PathBuf::from(lines.next().ok_or_else(|| parse_error(&stdout))?);
        let bare = lines.next().ok_or_else(|| parse_error(&stdout))? == "true";

        let work_dir = if bare {
            None
        } else {
            let output = git.run(
                &git.command(["rev-parse", "--path-format=absolute", "--show-toplevel"])
                    .cwd(path),
            )?;
            Some(PathBuf::from(output.stdout_lossy().trim_end_matches('\n')))
        };

        Ok(Self {
            git: git.clone(),
            git_dir,
            work_dir,
        })
    }

    /// Opens the repository whose work tree (or, for a bare repository, git
    /// dir) is exactly `path`. A subfolder of a repository is not accepted.
    pub fn open(git: &Git, path: &Path) -> Result<Self> {
        let repository = Self::discover(git, path)?;
        let wanted = canonical(path);
        let root = repository
            .work_dir
            .as_deref()
            .unwrap_or(&repository.git_dir);
        if canonical(root) == wanted {
            Ok(repository)
        } else {
            Err(Error::NotARepository(path.to_path_buf()))
        }
    }

    /// Creates a repository at `path` (creating the folder if needed) with no
    /// remote, and opens it.
    pub fn init(git: &Git, path: &Path, initial_branch: Option<&str>) -> Result<Self> {
        let mut args = vec!["init".to_owned(), "--quiet".to_owned()];
        if let Some(branch) = initial_branch {
            args.push(format!("--initial-branch={branch}"));
        }
        args.push("--".to_owned());
        args.push(path.to_string_lossy().into_owned());
        git.run(&git.command(args))?;
        Self::open(git, path)
    }

    #[must_use]
    pub fn git(&self) -> &Git {
        &self.git
    }

    /// The absolute path of the git directory, such as `<work tree>/.git`.
    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The work tree root; `None` for a bare repository.
    #[must_use]
    pub fn work_dir(&self) -> Option<&Path> {
        self.work_dir.as_deref()
    }

    #[must_use]
    pub fn is_bare(&self) -> bool {
        self.work_dir.is_none()
    }

    /// The folder Git commands for this repository run in.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.work_dir.as_deref().unwrap_or(&self.git_dir)
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn parse_error(stdout: &str) -> Error {
    Error::Parse(format!("rev-parse printed {stdout:?}"))
}
