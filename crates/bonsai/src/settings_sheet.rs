//! The Settings sheet. Every change applies and saves at once.

use bonsai_git::Git;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    settings::{self, AppearanceSetting, PullSetting, Settings},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        dialog::{dialog_footer, dialog_title},
        input::{Input, InputEvent, InputState},
        theme::UiTheme,
    },
};

pub enum SettingsEvent {
    Dismissed,
}

pub struct SettingsSheet {
    git_path: Entity<InputState>,
    editor: Entity<InputState>,
    terminal: Entity<InputState>,
    /// The Git the path field finds, or why it finds none.
    detected: Result<SharedString, SharedString>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsSheet {}

fn detect(path: &str) -> Result<SharedString, SharedString> {
    let configured = (!path.is_empty()).then(|| std::path::Path::new(path));
    Git::locate(configured)
        .map(|git| format!("Git {} at {}", git.version(), git.program().display()).into())
        .map_err(|error| error.to_string().into())
}

impl SettingsSheet {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = settings::get(cx);
        let field = |value: Option<String>,
                     placeholder: &'static str,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                if let Some(value) = value {
                    input.set_value(value, window, cx);
                }
                input
            })
        };
        let git_path = field(
            current.git_path.map(|p| p.display().to_string()),
            "git on PATH",
            window,
            cx,
        );
        let editor = field(
            current.editor,
            "System default (an app name, such as Zed)",
            window,
            cx,
        );
        let terminal = field(current.terminal, "Terminal", window, cx);
        let subscriptions = vec![
            cx.subscribe(&git_path, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().trim().to_owned();
                    this.detected = detect(&value);
                    if this.detected.is_ok() {
                        this.save(cx, |s| {
                            s.git_path = Some(value.into())
                                .filter(|p: &std::path::PathBuf| !p.as_os_str().is_empty());
                        });
                    }
                    cx.notify();
                }
            }),
            cx.subscribe(&editor, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().trim().to_owned();
                    this.save(cx, |s| s.editor = Some(value).filter(|v| !v.is_empty()));
                }
            }),
            cx.subscribe(&terminal, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().trim().to_owned();
                    this.save(cx, |s| s.terminal = Some(value).filter(|v| !v.is_empty()));
                }
            }),
        ];
        let detected = detect(git_path.read(cx).value().trim());
        Self {
            git_path,
            editor,
            terminal,
            detected,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn save(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Settings)) {
        self.error = settings::update(cx, change).err().map(SharedString::from);
        cx.notify();
    }

    #[must_use]
    pub fn detected(&self) -> &Result<SharedString, SharedString> {
        &self.detected
    }

    fn row(label: &'static str, cx: &mut Context<Self>) -> gpui_kit::Div {
        let theme = UiTheme::read(cx);
        div().flex().flex_col().gap(px(4.)).child(
            div()
                .text_xs()
                .text_color(theme.colors.muted_foreground)
                .child(label),
        )
    }
}

impl Render for SettingsSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let current = settings::get(cx);
        let choice = |id: &'static str, label: &'static str, selected: bool| {
            Button::new(id)
                .variant(if selected {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                })
                .size(ButtonSize::Sm)
                .label(label)
        };
        let appearance = div().flex().gap(px(4.)).children(
            [
                (
                    AppearanceSetting::System,
                    "System",
                    "settings.appearance.system",
                ),
                (
                    AppearanceSetting::Light,
                    "Light",
                    "settings.appearance.light",
                ),
                (AppearanceSetting::Dark, "Dark", "settings.appearance.dark"),
            ]
            .map(|(value, label, id)| {
                choice(id, label, current.appearance == value).on_click(
                    cx.listener(move |this, _, _, cx| this.save(cx, |s| s.appearance = value)),
                )
            }),
        );
        let pull = div().flex().gap(px(4.)).children(
            [
                (PullSetting::Merge, "Merge", "settings.pull.merge"),
                (PullSetting::Rebase, "Rebase", "settings.pull.rebase"),
                (
                    PullSetting::FastForwardOnly,
                    "Fast-forward only",
                    "settings.pull.ff",
                ),
            ]
            .map(|(value, label, id)| {
                choice(id, label, current.pull == value)
                    .on_click(cx.listener(move |this, _, _, cx| this.save(cx, |s| s.pull = value)))
            }),
        );
        let fetch = div().flex().gap(px(4.)).children(
            [
                (0, "Off", "settings.fetch.off"),
                (5, "5 min", "settings.fetch.5"),
                (10, "10 min", "settings.fetch.10"),
                (30, "30 min", "settings.fetch.30"),
            ]
            .map(|(minutes, label, id)| {
                choice(id, label, current.auto_fetch_minutes == minutes).on_click(cx.listener(
                    move |this, _, _, cx| this.save(cx, |s| s.auto_fetch_minutes = minutes),
                ))
            }),
        );
        let (detected, detected_color) = match &self.detected {
            Ok(found) => (format!("{found}. New tabs use it."), c.muted_foreground),
            Err(error) => (error.to_string(), c.destructive),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(dialog_title("settings.title", cx).child("Settings"))
            .child(Self::row("Appearance", cx).child(appearance))
            .child(Self::row("Pull", cx).child(pull))
            .child(Self::row("Fetch in the background", cx).child(fetch))
            .child(
                Self::row("Git", cx)
                    .child(Input::new(&self.git_path).aria_label("Git path"))
                    .child(div().text_xs().text_color(detected_color).child(detected)),
            )
            .child(Self::row("Editor", cx).child(Input::new(&self.editor).aria_label("Editor")))
            .child(
                Self::row("Terminal", cx).child(Input::new(&self.terminal).aria_label("Terminal")),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(c.destructive).child(error))
            })
            .child(
                dialog_footer(cx).child(
                    Button::new("settings.done")
                        .label("Done")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Dismissed))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;

    use super::*;

    #[gpui_kit::test]
    fn changes_apply_and_save(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let path = settings::settings_path(dir.path());
        cx.update(|cx| settings::install(Settings::default(), Some(path.clone()), cx));
        let (sheet, cx) = cx.add_window_view(SettingsSheet::new);
        sheet.read_with(cx, |sheet, _| assert!(sheet.detected().is_ok()));
        sheet.update(cx, |sheet, cx| {
            sheet.save(cx, |s| {
                s.pull = PullSetting::Rebase;
                s.auto_fetch_minutes = 0;
            });
        });
        assert_eq!(Settings::load(&path).pull, PullSetting::Rebase);
        cx.update(|_, cx| assert_eq!(settings::get(cx).auto_fetch_minutes, 0));

        let editor = sheet.read_with(cx, |sheet, _| sheet.editor.clone());
        editor.update_in(cx, |input, window, cx| {
            input.set_value("Zed", window, cx);
            cx.emit(InputEvent::Change);
        });
        cx.run_until_parked();
        assert_eq!(Settings::load(&path).editor.as_deref(), Some("Zed"));

        let git = sheet.read_with(cx, |sheet, _| sheet.git_path.clone());
        git.update_in(cx, |input, window, cx| {
            input.set_value("/no/such/git", window, cx);
            cx.emit(InputEvent::Change);
        });
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| assert!(sheet.detected().is_err()));
        // set_value emits no Change; typing does. A missing Git is not saved.
        assert_eq!(Settings::load(&path).git_path, None);
    }
}
