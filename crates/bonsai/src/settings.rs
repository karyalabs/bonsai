//! App settings (CONTEXT.md: Settings), saved as `settings.json` beside the
//! session and kept as a GPUI global.

use std::path::{Path, PathBuf};

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

use crate::ui::theme::{Appearance, UiTheme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppearanceSetting {
    #[default]
    System,
    Light,
    Dark,
}

impl AppearanceSetting {
    #[must_use]
    pub fn appearance(self) -> Appearance {
        match self {
            Self::System => Appearance::System,
            Self::Light => Appearance::Light,
            Self::Dark => Appearance::Dark,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PullSetting {
    #[default]
    Merge,
    Rebase,
    FastForwardOnly,
}

impl PullSetting {
    #[must_use]
    pub fn mode(self) -> bonsai_git::PullMode {
        match self {
            Self::Merge => bonsai_git::PullMode::Merge,
            Self::Rebase => bonsai_git::PullMode::Rebase,
            Self::FastForwardOnly => bonsai_git::PullMode::FastForwardOnly,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// A specific `git`; `None` uses the one on `PATH`.
    pub git_path: Option<PathBuf>,
    /// The app conflicted files open in; `None` is the system default.
    pub editor: Option<String>,
    /// The terminal app for Open in Terminal; `None` is Terminal.
    pub terminal: Option<String>,
    pub appearance: AppearanceSetting,
    /// Minutes between background fetches of open repositories; 0 is off.
    pub auto_fetch_minutes: u32,
    pub pull: PullSetting,
    /// Shows the Changes view as a folder tree.
    pub changes_tree: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            git_path: None,
            editor: None,
            terminal: None,
            appearance: AppearanceSetting::System,
            auto_fetch_minutes: 10,
            changes_tree: false,
            pull: PullSetting::Merge,
        }
    }
}

impl Settings {
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temp, path)
    }
}

#[must_use]
pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// The settings in effect, and where to save them (`None` in tests).
pub struct GlobalSettings {
    pub settings: Settings,
    pub path: Option<PathBuf>,
}

impl Global for GlobalSettings {}

/// The current settings; defaults before [`install`].
#[must_use]
pub fn get(cx: &App) -> Settings {
    cx.try_global::<GlobalSettings>()
        .map(|g| g.settings.clone())
        .unwrap_or_default()
}

/// Makes `settings` current and applies the appearance.
pub fn install(settings: Settings, path: Option<PathBuf>, cx: &mut App) {
    UiTheme::set_appearance(cx, settings.appearance.appearance());
    cx.set_global(GlobalSettings { settings, path });
}

/// Changes the settings, applies them and saves them.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let mut settings = get(cx);
    change(&mut settings);
    let path = cx
        .try_global::<GlobalSettings>()
        .and_then(|g| g.path.clone());
    install(settings.clone(), path.clone(), cx);
    match path {
        Some(path) => settings
            .save(&path)
            .map_err(|e| format!("Bonsai could not save its settings: {e}")),
        None => Ok(()),
    }
}

/// Opens `path` in the configured editor, or the system default.
pub fn open_in_editor(path: &Path, cx: &mut App) {
    match get(cx).editor.filter(|e| !e.trim().is_empty()) {
        Some(editor) if cfg!(target_os = "macos") => {
            let _ = std::process::Command::new("open")
                .arg("-a")
                .arg(editor.trim())
                .arg(path)
                .spawn();
        }
        Some(editor) => {
            let _ = std::process::Command::new(editor.trim()).arg(path).spawn();
        }
        None => cx.open_with_system(path),
    }
}

/// Opens a terminal in `folder`.
pub fn open_terminal(folder: &Path, cx: &App) -> Result<(), String> {
    let terminal = get(cx)
        .terminal
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Terminal".to_owned());
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg("-a")
            .arg(terminal.trim())
            .arg(folder)
            .spawn()
    } else {
        std::process::Command::new(
            std::env::var("TERMINAL").unwrap_or_else(|_| "x-terminal-emulator".to_owned()),
        )
        .current_dir(folder)
        .spawn()
    };
    result
        .map(|_| ())
        .map_err(|e| format!("Bonsai could not open {terminal}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_with_defaults_for_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_path(dir.path());
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::write(&path, r#"{"pull":"Rebase"}"#).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.pull, PullSetting::Rebase);
        assert_eq!(loaded.auto_fetch_minutes, 10);
        let changed = Settings {
            editor: Some("Zed".into()),
            appearance: AppearanceSetting::Dark,
            ..loaded
        };
        changed.save(&path).unwrap();
        assert_eq!(Settings::load(&path), changed);
    }
}
