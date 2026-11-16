//! The Settings sheet. Every change applies and saves at once.

use bonsai_git::Git;
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    settings::{self, AppearanceSetting, PullSetting, Settings},
    ui::{
        button::Button,
        dialog::{dialog_footer, dialog_title},
        input::{Input, InputEvent, InputState},
        theme::UiTheme,
        toggle_group::{ToggleGroup, ToggleGroupItem},
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

    /// A settings row: the label and a line on what it does on the left,
    /// the control on the right.
    fn row(
        label: &'static str,
        caption: impl Into<SharedString>,
        caption_color: Option<gpui_kit::Rgba>,
        control: impl IntoElement,
        theme: &UiTheme,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .items_center()
            .gap(px(16.))
            .px(px(14.))
            .py(px(10.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_sm().child(label))
                    .child(
                        div()
                            .text_xs()
                            .text_color(caption_color.unwrap_or(theme.colors.muted_foreground))
                            .child(caption.into()),
                    ),
            )
            .child(div().flex_none().child(control))
    }

    /// A field row: the label and caption above a full-width input.
    fn field_row(
        label: &'static str,
        caption: impl Into<SharedString>,
        caption_color: Option<gpui_kit::Rgba>,
        input: impl IntoElement,
        theme: &UiTheme,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(14.))
            .py(px(10.))
            .child(div().text_sm().child(label))
            .child(input)
            .child(
                div()
                    .text_xs()
                    .text_color(caption_color.unwrap_or(theme.colors.muted_foreground))
                    .child(caption.into()),
            )
    }

    fn section(
        title: &'static str,
        icon: LucideIcon,
        rows: Vec<gpui_kit::Div>,
        theme: &UiTheme,
    ) -> gpui_kit::Div {
        let c = &theme.colors;
        let mut card = div()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(c.border);
        for (i, row) in rows.into_iter().enumerate() {
            card = card.child(row.when(i > 0, |row| row.border_t_1().border_color(c.border)));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(c.muted_foreground)
                    .child(lucide(icon).size(px(12.)).text_color(c.muted_foreground))
                    .child(title.to_uppercase()),
            )
            .child(card)
    }
}

/// A segmented control over `options`, saving the picked one with `apply`.
fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: &'static str,
    current: T,
    options: &[(T, &'static str)],
    apply: impl Fn(&mut Settings, T) + Copy + 'static,
    cx: &mut Context<SettingsSheet>,
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
        let _ = this.update(cx, |sheet, cx| sheet.save(cx, |s| apply(s, value)));
    })
}

impl Render for SettingsSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let current = settings::get(cx);
        let appearance = segmented(
            "settings.appearance",
            "Appearance",
            current.appearance,
            &[
                (AppearanceSetting::System, "System"),
                (AppearanceSetting::Light, "Light"),
                (AppearanceSetting::Dark, "Dark"),
            ],
            |s, value| s.appearance = value,
            cx,
        );
        let pull = segmented(
            "settings.pull",
            "Pull",
            current.pull,
            &[
                (PullSetting::Merge, "Merge"),
                (PullSetting::Rebase, "Rebase"),
                (PullSetting::FastForwardOnly, "Fast-forward"),
            ],
            |s, value| s.pull = value,
            cx,
        );
        let fetch = segmented(
            "settings.fetch",
            "Fetch in the background",
            current.auto_fetch_minutes,
            &[(0, "Off"), (5, "5 min"), (10, "10 min"), (30, "30 min")],
            |s, value| s.auto_fetch_minutes = value,
            cx,
        );
        let (detected, detected_color) = match &self.detected {
            Ok(found) => (format!("{found}. New tabs use it."), None),
            Err(error) => (error.to_string(), Some(c.destructive)),
        };
        let pull_caption = match current.pull {
            PullSetting::Merge => "Merges the upstream into your branch when they diverge.",
            PullSetting::Rebase => "Replays your commits on top of the upstream.",
            PullSetting::FastForwardOnly => {
                "Only moves your branch forward; stops if they diverge."
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(dialog_title("settings.title", cx).child("Settings"))
            .child(Self::section(
                "General",
                LucideIcon::SlidersHorizontal,
                vec![Self::row(
                    "Appearance",
                    "Follow macOS or pick a side.",
                    None,
                    appearance,
                    &theme,
                )],
                &theme,
            ))
            .child(Self::section(
                "Syncing",
                LucideIcon::RefreshCw,
                vec![
                    Self::row("Pull", pull_caption, None, pull, &theme),
                    Self::row(
                        "Fetch in the background",
                        "Keeps ahead and behind counts current.",
                        None,
                        fetch,
                        &theme,
                    ),
                ],
                &theme,
            ))
            .child(Self::section(
                "Tools",
                LucideIcon::Wrench,
                vec![
                    Self::field_row(
                        "Git",
                        detected,
                        detected_color,
                        Input::new(&self.git_path).aria_label("Git path"),
                        &theme,
                    ),
                    Self::field_row(
                        "Editor",
                        "Open in Editor uses it. Leave empty for each file's default app.",
                        None,
                        Input::new(&self.editor).aria_label("Editor"),
                        &theme,
                    ),
                    Self::field_row(
                        "Terminal",
                        "Open in Terminal uses it.",
                        None,
                        Input::new(&self.terminal).aria_label("Terminal"),
                        &theme,
                    ),
                ],
                &theme,
            ))
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
                            .child("Changes save as you make them."),
                    )
                    .child(
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
