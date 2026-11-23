//! Repository Settings: this repository's committer identity, commit
//! signing, fetch pruning and hooks (CONTEXT.md: Repository settings).

use bonsai_git::{RepoSettings, RepoSettingsEdit, SigningChoice};
use gpui_icons::LucideIcon;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    repository::RepositoryModel,
    settings_sheet::SettingsSheet,
    ui::{
        button::{Button, ButtonVariant},
        dialog::{dialog_footer, dialog_title},
        input::{Input, InputState},
        switch::Switch,
        theme::UiTheme,
        toggle_group::{ToggleGroup, ToggleGroupItem},
    },
};

pub enum RepoSettingsEvent {
    Dismissed,
}

pub struct RepoSettingsSheet {
    model: Entity<RepositoryModel>,
    settings: RepoSettings,
    name: Entity<InputState>,
    email: Entity<InputState>,
    key: Entity<InputState>,
    signing: SigningChoice,
    prune: Option<bool>,
    /// The installed Git LFS version, if any.
    lfs_version: Option<String>,
    /// Patterns stored with Git LFS.
    lfs_patterns: Vec<String>,
    error: Option<SharedString>,
}

impl EventEmitter<RepoSettingsEvent> for RepoSettingsSheet {}

/// "Ada Lovelace (global)", for a placeholder showing what applies when
/// the field is empty.
fn inherited(value: Option<&str>, otherwise: &str) -> String {
    value.map_or_else(|| otherwise.to_owned(), |v| format!("{v} (global)"))
}

impl RepoSettingsSheet {
    pub fn new(
        model: Entity<RepositoryModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let read = model.read(cx).repository().repo_settings();
        let (settings, error) = match read {
            Ok(settings) => (settings, None),
            Err(error) => (RepoSettings::default(), Some(error.to_string().into())),
        };
        let field = |value: Option<&str>,
                     placeholder: String,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                if let Some(value) = value {
                    input.set_value(value.to_owned(), window, cx);
                }
                input
            })
        };
        let name = field(
            settings.name.local.as_deref(),
            inherited(settings.name.inherited.as_deref(), "No name set"),
            window,
            cx,
        );
        let email = field(
            settings.email.local.as_deref(),
            inherited(settings.email.inherited.as_deref(), "No email set"),
            window,
            cx,
        );
        let key = field(
            settings.key.local.as_deref(),
            inherited(settings.key.inherited.as_deref(), "Not set"),
            window,
            cx,
        );
        let signing = settings.signing();
        let repository = model.read(cx).repository();
        let lfs_version = repository.lfs_version();
        let lfs_patterns = repository.lfs_tracked().unwrap_or_default();
        let prune = settings
            .prune
            .local
            .as_deref()
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1"));
        Self {
            model,
            settings,
            name,
            email,
            key,
            signing,
            prune,
            lfs_version,
            lfs_patterns,
            error,
        }
    }

    #[must_use]
    pub fn settings(&self) -> &RepoSettings {
        &self.settings
    }

    pub fn set_signing(&mut self, signing: SigningChoice, cx: &mut Context<Self>) {
        self.signing = signing;
        cx.notify();
    }

    pub fn set_prune(&mut self, prune: Option<bool>, cx: &mut Context<Self>) {
        self.prune = prune;
        cx.notify();
    }

    /// The edit the fields describe.
    #[must_use]
    pub fn edit(&self, cx: &gpui_kit::App) -> RepoSettingsEdit {
        let text = |input: &Entity<InputState>| {
            Some(input.read(cx).value().trim().to_owned()).filter(|v| !v.is_empty())
        };
        RepoSettingsEdit {
            name: text(&self.name),
            email: text(&self.email),
            signing: self.signing,
            key: text(&self.key),
            prune: self.prune,
        }
    }

    /// Writes the fields to the repository's config and closes.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let edit = self.edit(cx);
        self.model.update(cx, |model, cx| {
            model.run(
                "Save repository settings",
                move |repository| repository.apply_repo_settings(&edit),
                cx,
            );
        });
        cx.emit(RepoSettingsEvent::Dismissed);
    }

    /// Turns a hook on or off right away.
    pub fn set_hook(&mut self, name: String, enabled: bool, cx: &mut Context<Self>) {
        if let Some(hook) = self.settings.hooks.iter_mut().find(|h| h.name == name) {
            hook.enabled = enabled;
        }
        let verb = if enabled { "Turn on" } else { "Turn off" };
        self.model.update(cx, |model, cx| {
            model.run(
                format!("{verb} the {name} hook"),
                move |repository| repository.set_hook_enabled(&name, enabled),
                cx,
            );
        });
        cx.notify();
    }

    #[must_use]
    pub fn lfs_patterns(&self) -> &[String] {
        &self.lfs_patterns
    }

    /// Stops storing files matching `pattern` with Git LFS, right away.
    pub fn untrack_lfs(&mut self, pattern: String, cx: &mut Context<Self>) {
        self.lfs_patterns.retain(|p| *p != pattern);
        self.model.update(cx, |model, cx| {
            model.run(
                format!("Stop storing {pattern} with Git LFS"),
                move |repository| repository.lfs_untrack(&pattern),
                cx,
            );
        });
        cx.notify();
    }

    /// Downloads the LFS files the checked-out commit needs.
    pub fn pull_lfs(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| {
            model.run_network(
                "Download Git LFS files",
                |repository, _, _| repository.lfs_pull(),
                cx,
            );
        });
    }

    fn lfs_rows(&self, theme: &UiTheme, cx: &mut Context<Self>) -> Vec<gpui_kit::Div> {
        let c = &theme.colors;
        let note = |text: String| {
            div()
                .px(px(14.))
                .py(px(10.))
                .text_xs()
                .text_color(c.muted_foreground)
                .child(text)
        };
        let Some(version) = &self.lfs_version else {
            return vec![note(
                "Git LFS isn't installed. Install it with `brew install git-lfs` to store large files outside the repository."
                    .to_owned(),
            )];
        };
        let mut rows = Vec::new();
        if self.lfs_patterns.is_empty() {
            rows.push(note(format!(
                "Nothing is stored with Git LFS {version} yet. In Changes, a file's Git LFS menu starts storing it."
            )));
        }
        for (index, pattern) in self.lfs_patterns.iter().enumerate() {
            let pattern = pattern.clone();
            rows.push(SettingsSheet::row(
                pattern.clone(),
                "Stored with Git LFS",
                None,
                Button::new(("repo-settings.lfs.untrack", index))
                    .label("Stop Storing")
                    .variant(ButtonVariant::Outline)
                    .on_click(cx.listener(move |sheet, _, _, cx| {
                        sheet.untrack_lfs(pattern.clone(), cx);
                    })),
                theme,
            ));
        }
        if self.lfs_patterns.is_empty() {
            return rows;
        }
        rows.push(SettingsSheet::row(
            "Download files",
            format!("Fetches the LFS files this checkout needs, with Git LFS {version}."),
            None,
            Button::new("repo-settings.lfs.pull")
                .label("Download")
                .variant(ButtonVariant::Outline)
                .on_click(cx.listener(|sheet, _, _, cx| sheet.pull_lfs(cx))),
            theme,
        ));
        rows
    }

    fn signing_caption(&self) -> String {
        match self.signing {
            SigningChoice::Inherit if self.settings.inherits_signing() => {
                let how = match self.settings.format.inherited.as_deref() {
                    Some("ssh") => "an SSH key",
                    Some("x509") => "an X.509 certificate",
                    _ => "GPG",
                };
                format!("Your global config signs commits with {how}.")
            }
            SigningChoice::Inherit => "Your global config doesn't sign commits.".to_owned(),
            SigningChoice::Off => "Commits here aren't signed.".to_owned(),
            SigningChoice::OpenPgp => "Commits here are signed with GPG.".to_owned(),
            SigningChoice::Ssh => "Commits here are signed with an SSH key.".to_owned(),
        }
    }
}

/// A segmented control over `options` that calls `pick` with the choice.
fn choice<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: &'static str,
    current: T,
    options: &[(T, &'static str)],
    pick: fn(&mut RepoSettingsSheet, T, &mut Context<RepoSettingsSheet>),
    cx: &mut Context<RepoSettingsSheet>,
) -> ToggleGroup {
    let selected = options
        .iter()
        .position(|(value, _)| *value == current)
        .map(|i| SharedString::from(i.to_string()));
    let values: Vec<T> = options.iter().map(|(value, _)| *value).collect();
    let mut group = ToggleGroup::new(id)
        .aria_label(label)
        .segmented(true)
        .value(selected);
    for (i, (_, text)) in options.iter().enumerate() {
        group = group.item(
            ToggleGroupItem::new(SharedString::from(format!("{id}.{i}")), i.to_string())
                .aria_label(*text)
                .child(*text),
        );
    }
    let this = cx.entity().downgrade();
    group.on_change(move |next, _, cx| {
        let Some(value) = next
            .first()
            .and_then(|i| i.parse::<usize>().ok())
            .and_then(|i| values.get(i).copied())
        else {
            return;
        };
        let _ = this.update(cx, |sheet, cx| pick(sheet, value, cx));
    })
}

impl Render for RepoSettingsSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let signing = choice(
            "repo-settings.signing",
            "Sign commits",
            self.signing,
            &[
                (SigningChoice::Inherit, "Global"),
                (SigningChoice::Off, "Off"),
                (SigningChoice::OpenPgp, "GPG"),
                (SigningChoice::Ssh, "SSH"),
            ],
            RepoSettingsSheet::set_signing,
            cx,
        );
        let prune = choice(
            "repo-settings.prune",
            "Prune on fetch",
            self.prune,
            &[(None, "Global"), (Some(true), "On"), (Some(false), "Off")],
            RepoSettingsSheet::set_prune,
            cx,
        );
        let prune_caption = match self.prune.or_else(|| {
            self.settings
                .prune
                .inherited
                .as_deref()
                .map(|v| v.eq_ignore_ascii_case("true"))
        }) {
            Some(true) => "Fetching removes remote branches deleted on the server.",
            _ => "Remote branches deleted on the server stay until you remove them.",
        };
        let signs = match self.signing {
            SigningChoice::Inherit => self.settings.inherits_signing(),
            SigningChoice::Off => false,
            SigningChoice::OpenPgp | SigningChoice::Ssh => true,
        };
        let mut signing_rows = vec![SettingsSheet::row(
            "Sign commits",
            self.signing_caption(),
            None,
            signing,
            &theme,
        )];
        if signs {
            signing_rows.push(SettingsSheet::field_row(
                "Signing key",
                if self.signing == SigningChoice::Ssh
                    || (self.signing == SigningChoice::Inherit
                        && self.settings.format.effective() == Some("ssh"))
                {
                    "A public key file, such as ~/.ssh/id_ed25519.pub, or the key itself."
                } else {
                    "A GPG key ID. Leave empty to use the key for your email."
                },
                None,
                Input::new(&self.key).aria_label("Signing key"),
                &theme,
            ));
        }
        let lfs_rows = self.lfs_rows(&theme, cx);
        let hooks_dir = self.settings.hooks_dir.display().to_string();
        let hook_rows: Vec<gpui_kit::Div> = if self.settings.hooks.is_empty() {
            vec![
                div()
                    .px(px(14.))
                    .py(px(10.))
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(format!(
                        "No hooks. Git runs scripts in {hooks_dir} before and after commits, pushes and more."
                    )),
            ]
        } else {
            self.settings
                .hooks
                .iter()
                .map(|hook| {
                    let name = hook.name.clone();
                    let caption = if hook.enabled {
                        "Runs"
                    } else {
                        "Off: not executable, so Git skips it"
                    };
                    SettingsSheet::row(
                        hook.name.clone(),
                        caption,
                        None,
                        Switch::new(SharedString::from(format!("repo-settings.hook.{name}")))
                            .checked(hook.enabled)
                            .aria_label(format!("Run the {name} hook"))
                            .on_change(cx.listener(move |sheet, checked: &bool, _, cx| {
                                sheet.set_hook(name.clone(), *checked, cx);
                            })),
                        &theme,
                    )
                })
                .collect()
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(dialog_title("repo-settings.title", cx).child("Repository Settings"))
            .child(
                // The sections scroll; the title and buttons stay put.
                div()
                    .id("repo-settings.body")
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .max_h(px(520.))
                    .overflow_y_scroll()
            .child(SettingsSheet::section(
                "Identity",
                LucideIcon::UserRound,
                vec![
                    SettingsSheet::field_row(
                        "Name",
                        "Commits in this repository use it. Leave empty to use your global name.",
                        None,
                        Input::new(&self.name).aria_label("Name"),
                        &theme,
                    ),
                    SettingsSheet::field_row(
                        "Email",
                        "Use a work email here and keep your personal one everywhere else.",
                        None,
                        Input::new(&self.email).aria_label("Email"),
                        &theme,
                    ),
                ],
                &theme,
            ))
            .child(SettingsSheet::section(
                "Signing",
                LucideIcon::ShieldCheck,
                signing_rows,
                &theme,
            ))
            .child(SettingsSheet::section(
                "Fetching",
                LucideIcon::RefreshCw,
                vec![SettingsSheet::row(
                    "Prune deleted branches",
                    prune_caption,
                    None,
                    prune,
                    &theme,
                )],
                &theme,
            ))
            .child(SettingsSheet::section(
                "Git LFS",
                LucideIcon::HardDrive,
                lfs_rows,
                &theme,
            ))
            .child(SettingsSheet::section(
                "Hooks",
                LucideIcon::Webhook,
                hook_rows,
                &theme,
            )))
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(c.destructive).child(error))
            })
            .child(
                dialog_footer(cx)
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child("Saved to this repository's .git/config."),
                    )
                    .child(
                        Button::new("repo-settings.cancel")
                            .label("Cancel")
                            .variant(ButtonVariant::Outline)
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(RepoSettingsEvent::Dismissed)),
                            ),
                    )
                    .child(
                        Button::new("repo-settings.save")
                            .label("Save")
                            .on_click(cx.listener(|sheet, _, _, cx| sheet.save(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Git, Repository};
    use gpui_kit::TestAppContext;

    use super::*;

    #[gpui_kit::test]
    fn saves_identity_and_signing_and_turns_hooks_off(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let git = Git::locate(None)
            .unwrap()
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
            .with_env("GIT_CONFIG_NOSYSTEM", "1");
        let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
        let hook = dir.path().join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let model = cx.new(|cx| RepositoryModel::new(repository.clone(), false, cx));
        let (sheet, cx) =
            cx.add_window_view(|window, cx| RepoSettingsSheet::new(model.clone(), window, cx));
        sheet.read_with(cx, |sheet, _| {
            assert_eq!(sheet.settings().hooks.len(), 1);
            assert!(sheet.settings().hooks[0].enabled);
        });

        let name = sheet.read_with(cx, |sheet, _| sheet.name.clone());
        name.update_in(cx, |input, window, cx| {
            input.set_value("Ada Lovelace", window, cx);
        });
        sheet.update(cx, |sheet, cx| {
            sheet.set_signing(SigningChoice::Ssh, cx);
            sheet.set_prune(Some(true), cx);
            sheet.set_hook("pre-commit".into(), false, cx);
            sheet.save(cx);
        });
        cx.run_until_parked();

        let settings = repository.repo_settings().unwrap();
        assert_eq!(settings.name.local.as_deref(), Some("Ada Lovelace"));
        assert_eq!(settings.email.local, None);
        assert_eq!(settings.signing(), SigningChoice::Ssh);
        assert_eq!(settings.prune.local.as_deref(), Some("true"));
        assert!(!settings.hooks[0].enabled);
    }
}
