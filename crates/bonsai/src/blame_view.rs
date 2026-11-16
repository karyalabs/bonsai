//! Blame: every line of a file beside the commit that last changed it.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use bonsai_git::{Blame, BlameCommit, Oid};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled,
    UniformListScrollHandle, Window, div, prelude::FluentBuilder as _, px, uniform_list,
};

use crate::{
    activity_panel::ago,
    avatar::avatar,
    repository::RepositoryModel,
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        theme::{ThemeMode, UiTheme},
        tooltip::text_tooltip,
    },
};

/// A row's height. Every row has one line, so the list can be uniform.
const ROW: f32 = 20.;
/// The annotation column: avatar, author, date and summary.
const ANNOTATION: f32 = 280.;

pub enum BlameEvent {
    /// Show this commit in History.
    ShowCommit(Oid),
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlameStatus {
    Loading,
    Ready,
    Failed(SharedString),
}

pub struct BlameView {
    path: PathBuf,
    /// The commit blamed at, or `None` for the work tree.
    rev: Option<Oid>,
    status: BlameStatus,
    blame: Arc<Blame>,
    /// The commit under the pointer; its lines are highlighted.
    hovered: Option<usize>,
    scroll: UniformListScrollHandle,
    /// The header button that closes Blame.
    close_label: &'static str,
}

impl EventEmitter<BlameEvent> for BlameView {}

impl BlameView {
    pub fn new(
        model: &Entity<RepositoryModel>,
        path: PathBuf,
        rev: Option<Oid>,
        cx: &mut Context<Self>,
    ) -> Self {
        let repository = model.read(cx).repository().clone();
        let (task_path, task_rev) = (path.clone(), rev.clone());
        let task = cx.background_spawn(async move {
            repository
                .blame(&task_path, task_rev.as_ref().map(Oid::as_str))
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(blame) => {
                        this.blame = Arc::new(blame);
                        this.status = BlameStatus::Ready;
                    }
                    Err(error) => this.status = BlameStatus::Failed(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            path,
            rev,
            status: BlameStatus::Loading,
            blame: Arc::default(),
            hovered: None,
            scroll: UniformListScrollHandle::new(),
            close_label: "Show Diff",
        }
    }

    /// Names the header button that closes Blame, "Show Diff" by default.
    #[must_use]
    pub fn close_label(mut self, label: &'static str) -> Self {
        self.close_label = label;
        self
    }

    #[must_use]
    pub fn status(&self) -> &BlameStatus {
        &self.status
    }

    #[must_use]
    pub fn blame(&self) -> &Blame {
        &self.blame
    }

    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    #[must_use]
    pub fn rev(&self) -> Option<&Oid> {
        self.rev.as_ref()
    }

    fn render_header(&self, theme: &UiTheme, cx: &mut Context<Self>) -> gpui_kit::Div {
        let c = &theme.colors;
        let name = self
            .path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let at = match &self.rev {
            Some(oid) => format!("at {}", oid.as_str().chars().take(7).collect::<String>()),
            None => "in the work tree".to_owned(),
        };
        let authors = {
            let mut names: Vec<&str> = self
                .blame
                .commits
                .iter()
                .filter(|commit| !commit.is_uncommitted())
                .map(|commit| commit.author_name.as_str())
                .collect();
            names.sort_unstable();
            names.dedup();
            names.len()
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(40.))
            .px(px(12.))
            .border_b_1()
            .border_color(c.border)
            .child(
                lucide(LucideIcon::ScrollText)
                    .size(px(15.))
                    .flex_none()
                    .text_color(c.muted_foreground),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(format!("Blame · {name}")),
            )
            .child(div().text_xs().text_color(c.muted_foreground).child(at))
            .when(self.status == BlameStatus::Ready, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!(
                            "· {}, {}, {}",
                            plural(self.blame.lines.len(), "line"),
                            plural(
                                self.blame
                                    .commits
                                    .iter()
                                    .filter(|commit| !commit.is_uncommitted())
                                    .count(),
                                "commit"
                            ),
                            plural(authors, "author")
                        )),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("blame.close")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .label(self.close_label)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(BlameEvent::Close))),
            )
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let dark = theme.mode == ThemeMode::Dark;
        let (oldest, newest) = self.blame.time_range();
        let span = (newest - oldest).max(1);
        let blame = self.blame.clone();
        let digits = u16::try_from(blame.lines.len().to_string().len()).unwrap_or(6);
        range
            .map(|index| {
                let line = &blame.lines[index];
                let commit = &blame.commits[line.commit];
                let first = blame.starts_run(index);
                let hovered = self.hovered == Some(line.commit);
                // Newer lines get a stronger bar, so recent work stands out.
                let heat = if commit.is_uncommitted() {
                    1.
                } else {
                    #[allow(clippy::cast_precision_loss)]
                    let age = (commit.author_time - oldest) as f32 / span as f32;
                    0.15 + 0.85 * age
                };
                let bar = if commit.is_uncommitted() {
                    c.warning
                } else {
                    c.primary.opacity(heat)
                };
                let commit_index = line.commit;
                div()
                    .id(("blame.row", index))
                    .h(px(ROW))
                    .flex()
                    .items_center()
                    .when(hovered, |this| this.bg(c.muted.opacity(0.6)))
                    .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                        let next = over.then_some(commit_index);
                        if *over || this.hovered == Some(commit_index) {
                            this.hovered = next;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .w(px(ANNOTATION))
                            .h_full()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .px(px(8.))
                            .border_r_1()
                            .border_color(c.border)
                            .when(first && index > 0, |this| {
                                this.border_t_1().border_color(c.border)
                            })
                            .when(first, |this| {
                                this.child(Self::annotation(commit, dark, &theme, cx))
                            }),
                    )
                    .child(div().w(px(3.)).h_full().flex_none().bg(bar))
                    .child(
                        div()
                            .w(px(8. * f32::from(digits) + 16.))
                            .flex_none()
                            .pr(px(8.))
                            .flex()
                            .justify_end()
                            .text_xs()
                            .font_family(theme.fonts.mono.clone())
                            .text_color(c.muted_foreground)
                            .child((index + 1).to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_xs()
                            .font_family(theme.fonts.mono.clone())
                            .child(line.text.replace('\t', "    ")),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The first row of a run: who, when and why, linking to the commit.
    fn annotation(
        commit: &BlameCommit,
        dark: bool,
        theme: &UiTheme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let c = &theme.colors;
        if commit.is_uncommitted() {
            return div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_xs()
                .text_color(c.warning)
                .child(
                    lucide(LucideIcon::Pencil)
                        .size(px(12.))
                        .text_color(c.warning),
                )
                .child("Not committed yet")
                .into_any_element();
        }
        let when = ago(UNIX_EPOCH + Duration::from_secs(commit.author_time.max(0).unsigned_abs()));
        let oid = commit.oid.clone();
        let tip = format!(
            "{}\n{} <{}>, {}\nClick to show the commit in History",
            commit.summary, commit.author_name, commit.author_email, when
        );
        div()
            .id(SharedString::from(format!(
                "blame.commit.{}",
                commit.oid.as_str()
            )))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .tooltip(move |_, cx| text_tooltip(tip.clone().into(), cx))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(BlameEvent::ShowCommit(oid.clone()))))
            .child(avatar(
                &commit.author_name,
                &commit.author_email,
                px(14.),
                dark,
            ))
            .child(
                div()
                    .w(px(78.))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .child(commit.author_name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(commit.summary.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(short_ago(&when)),
            )
            .into_any_element()
    }
}

/// "1 line", "2 lines".
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// "3 weeks ago" as "3w": the annotation column is narrow.
fn short_ago(text: &str) -> String {
    let mut parts = text.split(' ');
    match (parts.next(), parts.next()) {
        (Some("just"), _) => "now".to_owned(),
        (Some("yesterday"), _) => "1d".to_owned(),
        (Some("a"), Some("year")) => "1y".to_owned(),
        (Some(n), Some(unit)) => {
            let unit = match unit {
                "s" => "s",
                "min" => "m",
                "h" => "h",
                "days" => "d",
                "weeks" => "w",
                "months" => "mo",
                _ => "y",
            };
            format!("{n}{unit}")
        }
        _ => text.to_owned(),
    }
}

impl Render for BlameView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let header = self.render_header(&theme, cx);
        let body = match &self.status {
            BlameStatus::Loading => div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Reading the file's history…")
                .into_any_element(),
            BlameStatus::Failed(error) => div()
                .p(px(16.))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_sm()
                .text_color(c.destructive)
                .child(
                    lucide(LucideIcon::TriangleAlert)
                        .size(px(14.))
                        .flex_none()
                        .text_color(c.destructive),
                )
                .child(error.clone())
                .into_any_element(),
            BlameStatus::Ready => uniform_list(
                "blame.lines",
                self.blame.lines.len(),
                cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
            )
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::short_ago;

    #[test]
    fn shortens_relative_dates() {
        assert_eq!(short_ago("just now"), "now");
        assert_eq!(short_ago("5 min ago"), "5m");
        assert_eq!(short_ago("yesterday"), "1d");
        assert_eq!(short_ago("3 weeks ago"), "3w");
        assert_eq!(short_ago("2 months ago"), "2mo");
        assert_eq!(short_ago("a year ago"), "1y");
        assert_eq!(short_ago("4 years ago"), "4y");
    }
}
