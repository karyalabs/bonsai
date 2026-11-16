//! The files Git tracks, for the Files explorer.

use std::path::PathBuf;

use crate::{Repository, Result};

impl Repository {
    /// Every path in the index, sorted as Git sorts them. Paths deleted in
    /// the work tree but not yet staged are still listed.
    ///
    /// # Errors
    /// When `git ls-files` fails.
    pub fn tracked_files(&self) -> Result<Vec<PathBuf>> {
        self.files_at(None)
    }

    /// Every file in `rev`'s tree, or in the index for `None`.
    ///
    /// # Errors
    /// When Git can't list them, such as for an unknown revision.
    pub fn files_at(&self, rev: Option<&str>) -> Result<Vec<PathBuf>> {
        let args: Vec<&str> = match rev {
            Some(rev) => vec!["ls-tree", "-r", "-z", "--name-only", "--full-tree", rev],
            None => vec!["ls-files", "-z", "--full-name"],
        };
        let output = self.git().run(&self.git().command(args).cwd(self.root()))?;
        Ok(output
            .stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(crate::purge::bytes_to_path)
            .collect())
    }
}
