//! The activity log panel (CONTEXT.md: Activity log): the Git commands an
//! open repository ran, with their output, so a failure can be read in full
//! and any command copied into a terminal.

use std::time::{Duration, SystemTime};

use bonsai_git::CommandRecord;
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    ClipboardItem, Context, Entity, FontFeatures, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    repository::RepositoryModel,
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        checkbox::Checkbox,
        theme::UiTheme,
    },
};

/// Rows the list shows at most, newest first.
const SHOWN: usize = 300;

pub struct ActivityPanel {
    model: Entity<RepositoryModel>,
    /// The record shown in the detail, by its activity number.
    selected: Option<u64>,
    /// Also list commands that only read the repository.
    show_reads: bool,
    _model: Subscription,
}

impl ActivityPanel {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            selected: None,
            show_reads: false,
            _model: observe,
        }
    }

    #[must_use]
    pub fn selected(&self) -> Option<u64> {
        self.selected
    }

    pub fn select(&mut self, id: Option<u64>, cx: &mut Context<Self>) {
        self.selected = id;
        cx.notify();
    }

    pub fn set_show_reads(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_reads = show;
        cx.notify();
    }

    /// Selects the newest failed command, for "Show Details" on an error.
    /// A failed change comes before a failed read, since reads such as
    /// `config --get` fail routinely. Returns whether there was one.
    pub fn select_latest_failure(&mut self, cx: &mut Context<Self>) -> bool {
        let activity = self.model.read(cx).activity();
        let newest = |reads: bool| {
            activity
                .entries()
                .rev()
                .find(|(_, r)| !r.succeeded() && (reads || !r.is_read_only()))
                .map(|(id, _)| id)
        };
        let id = newest(false).or_else(|| newest(true));
        if id.is_some() {
            self.selected = id;
            cx.notify();
        }
        id.is_some()
    }

    /// The rows the list shows, newest first.
    #[must_use]
    pub fn rows(&self, cx: &gpui_kit::App) -> Vec<(u64, CommandRecord)> {
        let selected = self.selected;
        self.model
            .read(cx)
            .activity()
            .entries()
            .rev()
            // The selected row stays visible whatever the filter.
            .filter(|(id, record)| {
                self.show_reads || !record.is_read_only() || Some(*id) == selected
            })
            .take(SHOWN)
            .map(|(id, record)| (id, record.clone()))
            .collect()
    }

    fn render_detail(&self, record: &CommandRecord, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let line = record.command_line();
        let copy = line.clone();
        let result = if record.cancelled {
            "Cancelled".to_owned()
        } else {
            match record.exit_code {
                Some(0) => "Succeeded".to_owned(),
                Some(code) => format!("Failed with exit code {code}"),
                None => "Stopped by a signal".to_owned(),
            }
        };
        let meta = |label: &'static str, value: String| {
            div()
                .flex()
                .gap(px(8.))
                .text_xs()
                .child(
                    div()
                        .w(px(72.))
                        .flex_shrink_0()
                        .text_color(c.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(value),
                )
        };
        let output = |id: &'static str, label: &'static str, text: &str| {
            let text = text.trim_end().to_owned();
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().text_xs().text_color(c.muted_foreground).child(label))
                .child(
                    div()
                        .id(id)
                        .max_h(px(260.))
                        .overflow_y_scroll()
                        .p(px(8.))
                        .rounded(px(6.))
                        .bg(c.muted)
                        .font_family(theme.fonts.mono.clone())
                        .font_features(FontFeatures::disable_ligatures())
                        .text_xs()
                        .whitespace_normal()
                        .child(if text.is_empty() {
                            "(empty)".to_owned()
                        } else {
                            text
                        }),
                )
        };
        div()
            .id("activity.detail")
            .size_full()
            .overflow_y_scroll()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(theme.fonts.mono.clone())
                            .font_features(FontFeatures::disable_ligatures())
                            .text_sm()
                            .child(line),
                    )
                    .child(
                        Button::new("activity.copy")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Copy Command")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(meta("Result", result))
                    .child(meta("Took", duration(record.duration)))
                    .child(meta("Started", ago(record.started_at)))
                    .when_some(record.cwd.as_ref(), |this, cwd| {
                        this.child(meta("Folder", cwd.display().to_string()))
                    }),
            )
            .when(
                !record.stderr.trim().is_empty() || !record.succeeded(),
                |this| {
                    this.child(output(
                        "activity.stderr",
                        "Messages (stderr)",
                        &record.stderr,
                    ))
                },
            )
            .child(output("activity.stdout", "Output (stdout)", &record.stdout))
    }
}

/// A short duration: "840 ms", "2.4 s", "3 min 5 s".
#[must_use]
pub fn duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", d.as_secs_f64())
    } else {
        let s = d.as_secs();
        format!("{} min {} s", s / 60, s % 60)
    }
}

/// How long ago `at` was: "just now", "4 min ago", "yesterday", "5 weeks ago".
#[must_use]
pub fn ago(at: SystemTime) -> String {
    let s = SystemTime::now()
        .duration_since(at)
        .unwrap_or_default()
        .as_secs();
    match s {
        0..10 => "just now".to_owned(),
        10..60 => format!("{s} s ago"),
        60..3_600 => format!("{} min ago", s / 60),
        3_600..86_400 => format!("{} h ago", s / 3_600),
        86_400..172_800 => "yesterday".to_owned(),
        172_800..1_209_600 => format!("{} days ago", s / 86_400),
        1_209_600..5_184_000 => format!("{} weeks ago", s / 604_800),
        5_184_000..31_536_000 => format!("{} months ago", s / 2_592_000),
        _ => match s / 31_536_000 {
            1 => "a year ago".to_owned(),
            years => format!("{years} years ago"),
        },
    }
}

impl Render for ActivityPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let rows = self.rows(cx);
        let selected = self
            .selected
            .and_then(|id| self.model.read(cx).activity().get(id).cloned());
        let mut list = div()
            .id("activity.list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();
        if rows.is_empty() {
            list = list.child(
                div()
                    .p(px(16.))
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child("Commands Bonsai runs in this repository appear here."),
            );
        }
        for (id, record) in rows {
            let (icon, color) = if record.cancelled {
                (LucideIcon::CircleSlash, c.muted_foreground)
            } else if record.succeeded() {
                (LucideIcon::CircleCheck, c.success)
            } else {
                (LucideIcon::CircleX, c.destructive)
            };
            let is_selected = self.selected == Some(id);
            list = list.child(
                div()
                    .id(SharedString::from(format!("activity.row.{id}")))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(c.border)
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(c.muted))
                    .hover(|s| s.bg(c.muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(Some(id), cx)))
                    .child(lucide(icon).size(px(14.)).text_color(color).flex_shrink_0())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(theme.fonts.mono.clone())
                            .font_features(FontFeatures::disable_ligatures())
                            .text_xs()
                            .child(record.command_line()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(format!(
                                "{} · {}",
                                duration(record.duration),
                                ago(record.started_at)
                            )),
                    ),
            );
        }
        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.))
            .py(px(8.))
            .border_b_1()
            .border_color(c.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Activity"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_xs()
                    .child(
                        Checkbox::new("activity.show-reads")
                            .checked(self.show_reads)
                            .aria_label("Show read-only commands")
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_show_reads(*checked, cx);
                            })),
                    )
                    .child("Show reads"),
            );
        let detail = match &selected {
            Some(record) => self.render_detail(record, cx).into_any_element(),
            None => div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Select a command to see its output.")
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .w(px(460.))
                    .flex_shrink_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(c.border)
                    .child(toolbar)
                    .child(list),
            )
            .child(div().flex_1().min_w_0().h_full().child(detail))
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Git, Repository};
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::*;

    #[gpui_kit::test]
    fn lists_changes_hides_reads_and_finds_the_failure(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let repository =
            Repository::init(&Git::locate(None).unwrap(), dir.path(), Some("main")).unwrap();
        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (panel, cx) = cx.add_window_view(|_, cx| ActivityPanel::new(model.clone(), cx));
        cx.run_until_parked();

        model.update(cx, |model, cx| {
            model.run(
                "Add a remote",
                |repository| {
                    repository.add_remote("origin", "https://ada:hunter2@example.com/a.git")
                },
                cx,
            );
            model.run(
                "Delete a missing branch",
                |repository| repository.delete_branch("missing", false),
                cx,
            );
        });
        cx.run_until_parked();

        let lines = |panel: &Entity<ActivityPanel>, cx: &mut gpui_kit::VisualTestContext| {
            panel.read_with(cx, |panel, cx| {
                panel
                    .rows(cx)
                    .iter()
                    .map(|(_, r)| r.command_line())
                    .collect::<Vec<_>>()
            })
        };
        let changes = lines(&panel, cx);
        // Snapshot reads are hidden; the two changes show, newest first.
        assert_eq!(changes.len(), 2, "{changes:?}");
        assert!(changes[0].starts_with("git branch -d -- missing"));
        assert!(changes[1].starts_with("git remote add -- origin"));
        // Credentials never reach the log.
        assert!(changes.iter().all(|l| !l.contains("hunter2")));

        panel.update(cx, |panel, cx| panel.set_show_reads(true, cx));
        assert!(lines(&panel, cx).len() > 2);

        let found = panel.update(cx, ActivityPanel::select_latest_failure);
        assert!(found);
        panel.read_with(cx, |panel, cx| {
            let record = model
                .read(cx)
                .activity()
                .get(panel.selected().unwrap())
                .unwrap();
            assert_eq!(record.args[..2], ["branch", "-d"]);
            assert!(!record.succeeded());
        });
    }

    #[test]
    fn durations_and_ages_read_naturally() {
        assert_eq!(duration(Duration::from_millis(840)), "840 ms");
        assert_eq!(duration(Duration::from_millis(2_400)), "2.4 s");
        assert_eq!(duration(Duration::from_secs(185)), "3 min 5 s");
        assert_eq!(ago(SystemTime::now()), "just now");
        assert_eq!(
            ago(SystemTime::now() - Duration::from_secs(7_200)),
            "2 h ago"
        );
        let days = |n: u64| ago(SystemTime::now() - Duration::from_secs(n * 86_400 + 60));
        assert_eq!(days(1), "yesterday");
        assert_eq!(days(4), "4 days ago");
        assert_eq!(days(35), "5 weeks ago");
        assert_eq!(days(100), "3 months ago");
        assert_eq!(days(400), "a year ago");
        assert_eq!(days(800), "2 years ago");
    }
}
