//! Settings that apply to one repository: its committer identity, commit
//! signing, fetch pruning and hooks (CONTEXT.md: Repository settings).
//!
//! Each value is written to the repository's own config, overriding the
//! global one; clearing it falls back to the global value again.

use std::path::{Path, PathBuf};

use crate::{Error, Repository, Result};

/// One config value: what this repository sets, and what it would inherit
/// from the global or system config without it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigValue {
    pub local: Option<String>,
    pub inherited: Option<String>,
}

impl ConfigValue {
    /// The value Git uses.
    #[must_use]
    pub fn effective(&self) -> Option<&str> {
        self.local.as_deref().or(self.inherited.as_deref())
    }
}

/// How commits made in this repository are signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigningChoice {
    /// Whatever the global config says.
    Inherit,
    Off,
    OpenPgp,
    Ssh,
}

/// A script Git runs at a point such as before a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    /// Such as `pre-commit`.
    pub name: String,
    pub path: PathBuf,
    /// Executable, so Git runs it.
    pub enabled: bool,
}

/// This repository's settings, as read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoSettings {
    pub name: ConfigValue,
    pub email: ConfigValue,
    /// `commit.gpgsign`.
    pub sign: ConfigValue,
    /// `gpg.format`: `openpgp`, `ssh` or `x509`.
    pub format: ConfigValue,
    /// `user.signingkey`.
    pub key: ConfigValue,
    /// `fetch.prune`.
    pub prune: ConfigValue,
    /// Where Git looks for hooks.
    pub hooks_dir: PathBuf,
    pub hooks: Vec<Hook>,
}

impl RepoSettings {
    /// The signing choice this repository makes for itself.
    #[must_use]
    pub fn signing(&self) -> SigningChoice {
        match self.sign.local.as_deref().map(is_true) {
            None => SigningChoice::Inherit,
            Some(false) => SigningChoice::Off,
            Some(true) => match self.format.effective() {
                Some("ssh") => SigningChoice::Ssh,
                _ => SigningChoice::OpenPgp,
            },
        }
    }

    /// Whether commits are signed without this repository's own choice.
    #[must_use]
    pub fn inherits_signing(&self) -> bool {
        self.sign.inherited.as_deref().is_some_and(is_true)
    }
}

/// Changes to make; `None` fields clear the repository's own value, so the
/// global one applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSettingsEdit {
    pub name: Option<String>,
    pub email: Option<String>,
    pub signing: SigningChoice,
    pub key: Option<String>,
    pub prune: Option<bool>,
}

fn is_true(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    )
}

/// Hooks Git knows, so stray files in the folder aren't listed.
const HOOKS: [&str; 20] = [
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
];

/// Reads `git config --show-scope --list` output into a value for `key`.
fn config_value(list: &str, key: &str) -> ConfigValue {
    let mut value = ConfigValue::default();
    for line in list.lines() {
        let Some((scope, entry)) = line.split_once('\t') else {
            continue;
        };
        let Some((name, text)) = entry.split_once('=') else {
            continue;
        };
        if !name.eq_ignore_ascii_case(key) {
            continue;
        }
        // Later entries win, as in Git.
        if scope == "local" || scope == "worktree" {
            value.local = Some(text.to_owned());
        } else {
            value.inherited = Some(text.to_owned());
        }
    }
    value
}

impl Repository {
    fn config_git<I, S>(&self, args: I) -> Result<crate::Output>
    where
        I: IntoIterator<Item = S>,
        S: Into<std::ffi::OsString>,
    {
        self.git()
            .run_unchecked(&self.git().command(args).cwd(self.root()))
    }

    /// Reads this repository's settings and the hooks it has.
    pub fn repo_settings(&self) -> Result<RepoSettings> {
        let list = self
            .config_git(["config", "--show-scope", "--list"])?
            .stdout_lossy();
        let hooks_dir = self.hooks_dir()?;
        Ok(RepoSettings {
            name: config_value(&list, "user.name"),
            email: config_value(&list, "user.email"),
            sign: config_value(&list, "commit.gpgsign"),
            format: config_value(&list, "gpg.format"),
            key: config_value(&list, "user.signingkey"),
            prune: config_value(&list, "fetch.prune"),
            hooks: list_hooks(&hooks_dir),
            hooks_dir,
        })
    }

    /// Writes `edit` to this repository's config.
    pub fn apply_repo_settings(&self, edit: &RepoSettingsEdit) -> Result<()> {
        let clean = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        self.set_local_config("user.name", clean(&edit.name).as_deref())?;
        self.set_local_config("user.email", clean(&edit.email).as_deref())?;
        self.set_local_config("user.signingkey", clean(&edit.key).as_deref())?;
        let (sign, format) = match edit.signing {
            SigningChoice::Inherit => (None, None),
            SigningChoice::Off => (Some("false"), None),
            SigningChoice::OpenPgp => (Some("true"), Some("openpgp")),
            SigningChoice::Ssh => (Some("true"), Some("ssh")),
        };
        self.set_local_config("commit.gpgsign", sign)?;
        self.set_local_config("gpg.format", format)?;
        self.set_local_config(
            "fetch.prune",
            edit.prune.map(|on| if on { "true" } else { "false" }),
        )?;
        Ok(())
    }

    /// Sets `key` in this repository's config, or removes it.
    pub fn set_local_config(&self, key: &str, value: Option<&str>) -> Result<()> {
        let output = match value {
            Some(value) => self.config_git(["config", "--local", key, value])?,
            None => self.config_git(["config", "--local", "--unset-all", key])?,
        };
        // 5: there was nothing to unset.
        match output.status.code() {
            Some(0) => Ok(()),
            Some(5) if value.is_none() => Ok(()),
            _ => Err(Error::Refused(format!(
                "Git couldn't set {key}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }

    /// The folder Git runs hooks from, honouring `core.hooksPath`.
    pub fn hooks_dir(&self) -> Result<PathBuf> {
        let output = self.config_git(["rev-parse", "--git-path", "hooks"])?;
        let path = PathBuf::from(output.stdout_lossy().trim());
        Ok(if path.is_absolute() {
            path
        } else {
            self.root().join(path)
        })
    }

    /// Turns a hook on or off by making it executable or not; its script
    /// stays where it is.
    pub fn set_hook_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        if !HOOKS.contains(&name) {
            return Err(Error::Refused(format!("{name} isn't a Git hook.")));
        }
        let path = self.hooks_dir()?.join(name);
        set_executable(&path, enabled).map_err(|source| Error::Io { path, source })
    }
}

fn list_hooks(dir: &Path) -> Vec<Hook> {
    let mut hooks: Vec<Hook> = HOOKS
        .iter()
        .filter_map(|name| {
            let path = dir.join(name);
            let metadata = std::fs::metadata(&path)
                .ok()
                .filter(std::fs::Metadata::is_file)?;
            Some(Hook {
                name: (*name).to_owned(),
                enabled: is_executable(&metadata),
                path,
            })
        })
        .collect();
    hooks.sort_by(|a, b| a.name.cmp(&b.name));
    hooks
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    let mode = permissions.mode();
    permissions.set_mode(if executable {
        mode | ((mode & 0o444) >> 2)
    } else {
        mode & !0o111
    });
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_executable(_: &Path, _: bool) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "Hooks are turned on and off by their executable bit.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_values_override_inherited_ones() {
        let list = "system\tuser.name=Root\nglobal\tuser.name=Ada Lovelace\nlocal\tuser.name=Ada\nglobal\tcommit.gpgsign=true\n";
        let name = config_value(list, "user.name");
        assert_eq!(name.local.as_deref(), Some("Ada"));
        assert_eq!(name.inherited.as_deref(), Some("Ada Lovelace"));
        assert_eq!(name.effective(), Some("Ada"));
        let settings = RepoSettings {
            sign: config_value(list, "commit.gpgsign"),
            ..RepoSettings::default()
        };
        assert_eq!(settings.signing(), SigningChoice::Inherit);
        assert!(settings.inherits_signing());
    }
}
