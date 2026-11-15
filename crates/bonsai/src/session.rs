//! What Bonsai restores on launch: open tabs and pane sizes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_SIDEBAR_WIDTH: f32 = 240.;
pub const DEFAULT_DETAIL_WIDTH: f32 = 520.;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Work tree (or bare git dir) paths of the open tabs, in order.
    pub tabs: Vec<PathBuf>,
    pub active_tab: usize,
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    /// Width of the center list pane; the detail pane takes the rest.
    pub list_width: f32,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active_tab: 0,
            sidebar_open: true,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            list_width: DEFAULT_DETAIL_WIDTH,
        }
    }
}

impl Session {
    /// Reads the session, falling back to the default when the file is
    /// missing or unreadable.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes atomically: a crash never leaves a half-written file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temp, path)
    }
}

/// Bonsai's folder for settings, recents and the session:
/// `~/Library/Application Support/Bonsai` on macOS, the XDG data folder
/// elsewhere. `BONSAI_DATA_DIR` overrides it (tests, development).
#[must_use]
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BONSAI_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Bonsai")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"))
            .join("bonsai")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_missing_or_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/session.json");
        assert_eq!(Session::load(&path), Session::default());

        let session = Session {
            tabs: vec!["/a".into(), "/b".into()],
            active_tab: 1,
            sidebar_open: false,
            sidebar_width: 300.,
            list_width: 400.,
        };
        session.save(&path).unwrap();
        assert_eq!(Session::load(&path), session);

        std::fs::write(&path, r#"{"tabs":["/x"]}"#).unwrap();
        let partial = Session::load(&path);
        assert_eq!(partial.tabs, [PathBuf::from("/x")]);
        assert!(partial.sidebar_open);

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Session::load(&path), Session::default());
    }
}
