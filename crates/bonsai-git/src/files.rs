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
        let output = self.git().run(
            &self
                .git()
                .command(["ls-files", "-z", "--full-name"])
                .cwd(self.root()),
        )?;
        Ok(output
            .stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(crate::purge::bytes_to_path)
            .collect())
    }
}
