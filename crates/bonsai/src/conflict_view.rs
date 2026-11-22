//! The conflict resolver: a conflicted file's marked regions one at a time,
//! each kept from the current side, the incoming side, both or neither,
//! then saved and marked resolved.

use std::path::PathBuf;

use bonsai_git::{ConflictKind, Resolution, Segment, Side, side_name};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Task, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    repository::RepositoryModel,
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        menu::{Menu, MenuItem, MenuState},
        theme::UiTheme,
    },
    workspace::title_bar::ghost_trigger,
};

/// Unchanged lines shown on each side of a conflict before the rest fold.
const CONTEXT: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Loading,
    /// The file's regions; no conflicts means the markers are gone or the
    /// conflict isn't about the text.
    Ready(Vec<Segment>),
    Failed(SharedString),
}

pub struct ConflictView {
    model: Entity<RepositoryModel>,
    path: Option<PathBuf>,
    kind: Option<ConflictKind>,
    status: Status,
    /// Each conflict's chosen resolution, in file order.
    resolutions: Vec<Option<Resolution>>,
    /// Each conflict's "more" menu.
    menus: Vec<Entity<MenuState>>,
    /// Whether the detail pane shows the resolver instead of the diff.
    shown: bool,
    load: Option<Task<()>>,
}

impl ConflictView {
    #[must_use]
    pub fn new(model: Entity<RepositoryModel>) -> Self {
        Self {
            model,
            path: None,
            kind: None,
            status: Status::Idle,
            resolutions: Vec::new(),
            menus: Vec::new(),
            shown: false,
            load: None,
        }
    }

    #[must_use]
    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    #[must_use]
    pub fn resolutions(&self) -> &[Option<Resolution>] {
        &self.resolutions
    }

    /// How many conflicts the file has.
    #[must_use]
    pub fn conflicts(&self) -> usize {
        match &self.status {
            Status::Ready(segments) => segments
                .iter()
                .filter(|s| matches!(s, Segment::Conflict(_)))
                .count(),
            _ => 0,
        }
    }

    #[must_use]
    pub fn resolved(&self) -> usize {
        self.resolutions.iter().flatten().count()
    }

    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// Gives the detail pane back to the diff.
    pub fn hide(&mut self) {
        self.shown = false;
    }

    /// Shows the conflicted file at `path`, reading it again.
    pub fn show(&mut self, path: PathBuf, kind: ConflictKind, cx: &mut Context<Self>) {
        self.shown = true;
        let same = self.path.as_ref() == Some(&path);
        self.path = Some(path.clone());
        self.kind = Some(kind);
        if !same {
            self.status = Status::Loading;
            self.resolutions.clear();
        }
        let repository = self.model.read(cx).repository().clone();
        let task = cx.background_spawn(async move { repository.read_conflicts(&path) });
        self.load = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(segments) => {
                        let count = segments
                            .iter()
                            .filter(|s| matches!(s, Segment::Conflict(_)))
                            .count();
                        // Keep choices made before a reread of the same file.
                        if !same || this.resolutions.len() != count {
                            this.resolutions = vec![None; count];
                        }
                        this.status = Status::Ready(segments);
                    }
                    Err(error) => this.status = Status::Failed(error.to_string().into()),
                }
                this.rebuild_menus(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Chooses how conflict `index` is resolved, or undoes the choice.
    pub fn resolve(
        &mut self,
        index: usize,
        resolution: Option<Resolution>,
        cx: &mut Context<Self>,
    ) {
        if let Some(slot) = self.resolutions.get_mut(index) {
            *slot = resolution;
            cx.notify();
        }
    }

    /// Resolves every conflict left with one side.
    pub fn resolve_rest(&mut self, resolution: &Resolution, cx: &mut Context<Self>) {
        for slot in &mut self.resolutions {
            if slot.is_none() {
                *slot = Some(resolution.clone());
            }
        }
        cx.notify();
    }

    /// Writes the file with the chosen resolutions. When every conflict has
    /// one, the file is marked resolved too.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Status::Ready(segments)) = (self.path.clone(), &self.status) else {
            return;
        };
        let segments = segments.clone();
        let resolutions = self.resolutions.clone();
        let done = resolutions.iter().all(Option::is_some);
        let name = format!(
            "{} {}",
            if done {
                "Resolve"
            } else {
                "Save resolutions in"
            },
            path.display()
        );
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.write_resolution(&path, &segments, &resolutions),
                cx,
            );
        });
        if !done {
            // The saved regions are plain text now; what's left is what
            // the written file holds.
            let Status::Ready(segments) = &self.status else {
                return;
            };
            let text = bonsai_git::resolve_conflicts(segments, &self.resolutions);
            let segments = bonsai_git::parse_conflicts(&text);
            let left = segments
                .iter()
                .filter(|s| matches!(s, Segment::Conflict(_)))
                .count();
            self.status = Status::Ready(segments);
            self.resolutions = vec![None; left];
            self.rebuild_menus(cx);
            cx.notify();
        }
    }

    /// Resolves the whole file with one side's version, for conflicts that
    /// aren't about the text, such as one side deleting the file.
    pub fn take_side(&mut self, side: Side, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let name = format!(
            "Keep {} version of {}",
            if side == Side::Ours { "our" } else { "their" },
            path.display()
        );
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.resolve_with(&[path], side),
                cx,
            );
        });
    }

    fn operation(&self, cx: &gpui_kit::App) -> Option<bonsai_git::InProgress> {
        self.model.read(cx).snapshot().and_then(|s| s.operation)
    }

    fn rebuild_menus(&mut self, cx: &mut Context<Self>) {
        let operation = self.operation(cx);
        let ours = side_name(Side::Ours, operation);
        let theirs = side_name(Side::Theirs, operation);
        let this = cx.entity().downgrade();
        self.menus = (0..self.resolutions.len())
            .map(|index| {
                let pick = |resolution: Resolution| {
                    let this = this.clone();
                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                        let _ = this.update(cx, |view, cx| {
                            view.resolve(index, Some(resolution.clone()), cx);
                        });
                    }
                };
                let items = vec![
                    MenuItem::new(
                        "conflict.both-theirs-first",
                        format!("Use Both, {theirs} First"),
                    )
                    .icon(LucideIcon::ArrowUpDown)
                    .on_click(pick(Resolution::BothTheirsFirst)),
                    MenuItem::new("conflict.neither", "Use Neither")
                        .icon(LucideIcon::X)
                        .on_click(pick(Resolution::Neither)),
                    MenuItem::separator(),
                    MenuItem::new("conflict.rest-ours", format!("Use {ours} for All Left"))
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| {
                                let _ = this.update(cx, |view, cx| {
                                    view.resolve_rest(&Resolution::Ours, cx);
                                });
                            }
                        }),
                    MenuItem::new("conflict.rest-theirs", format!("Use {theirs} for All Left"))
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| {
                                let _ = this.update(cx, |view, cx| {
                                    view.resolve_rest(&Resolution::Theirs, cx);
                                });
                            }
                        }),
                ];
                cx.new(|cx| MenuState::new(items, cx))
            })
            .collect();
    }
}

fn resolution_name(resolution: &Resolution, ours: &str, theirs: &str) -> String {
    match resolution {
        Resolution::Ours => format!("Kept {ours}"),
        Resolution::Theirs => format!("Kept {theirs}"),
        Resolution::Both => format!("Kept both, {ours} first"),
        Resolution::BothTheirsFirst => format!("Kept both, {theirs} first"),
        Resolution::Neither => "Removed both".to_owned(),
    }
}

/// Lines of code in a tinted box, with a colored edge.
fn code_block(
    lines: &[String],
    edge: gpui_kit::Rgba,
    tint: gpui_kit::Hsla,
    theme: &UiTheme,
) -> impl IntoElement + use<> {
    let c = &theme.colors;
    let mut block = div()
        .flex()
        .flex_col()
        .border_l_2()
        .border_color(edge)
        .bg(tint)
        .py(px(4.))
        .font_family(theme.fonts.mono.clone())
        .text_xs();
    if lines.is_empty() {
        block = block.child(
            div()
                .px(px(10.))
                .italic()
                .text_color(c.muted_foreground)
                .child("No lines"),
        );
    }
    for line in lines {
        let text = line.trim_end_matches(['\n', '\r']).replace('\t', "    ");
        block = block.child(div().px(px(10.)).min_h(px(18.)).whitespace_nowrap().child(
            if text.is_empty() {
                " ".to_owned()
            } else {
                text
            },
        ));
    }
    block
}

impl ConflictView {
    fn render_common(
        lines: &[String],
        first: bool,
        last: bool,
        index: usize,
        theme: &UiTheme,
    ) -> impl IntoElement + use<> {
        let c = &theme.colors;
        let head = if first { 0 } else { CONTEXT.min(lines.len()) };
        let tail = if last {
            0
        } else {
            CONTEXT.min(lines.len().saturating_sub(head))
        };
        let folded = lines.len() - head - tail;
        let plain = |lines: &[String]| {
            let mut block = div()
                .flex()
                .flex_col()
                .py(px(2.))
                .font_family(theme.fonts.mono.clone())
                .text_xs()
                .text_color(c.muted_foreground);
            for line in lines {
                let text = line.trim_end_matches(['\n', '\r']).replace('\t', "    ");
                block = block.child(div().px(px(12.)).min_h(px(18.)).whitespace_nowrap().child(
                    if text.is_empty() {
                        " ".to_owned()
                    } else {
                        text
                    },
                ));
            }
            block
        };
        div()
            .id(("conflict.common", index))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .child(plain(&lines[..head]))
            .when(folded > 0, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(12.))
                        .py(px(2.))
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(lucide(LucideIcon::Ellipsis).size(px(12.)))
                        .child(if folded == 1 {
                            "1 unchanged line".to_owned()
                        } else {
                            format!("{folded} unchanged lines")
                        }),
                )
            })
            .child(plain(&lines[lines.len() - tail..]))
    }

    #[allow(clippy::too_many_lines)]
    fn render_conflict(
        &self,
        index: usize,
        conflict: &bonsai_git::Conflict,
        ours: &'static str,
        theirs: &'static str,
        theme: &UiTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = &theme.colors;
        let resolution = self.resolutions.get(index).cloned().flatten();
        let ours_color = c.success;
        let theirs_color = c.info;
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .px(px(10.))
            .py(px(6.))
            .border_b_1()
            .border_color(c.border)
            .bg(c.muted.opacity(0.5))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(
                        lucide(if resolution.is_some() {
                            LucideIcon::CircleCheck
                        } else {
                            LucideIcon::TriangleAlert
                        })
                        .size(px(13.))
                        .text_color(if resolution.is_some() {
                            c.success
                        } else {
                            c.warning
                        }),
                    )
                    .child(format!("Conflict {}", index + 1))
                    .when_some(resolution.clone(), |this, resolution| {
                        this.child(
                            div()
                                .font_weight(gpui_kit::FontWeight::NORMAL)
                                .text_color(c.muted_foreground)
                                .child(resolution_name(&resolution, ours, theirs)),
                        )
                    }),
            )
            .child(if resolution.is_some() {
                div()
                    .child(
                        Button::new(("conflict.undo", index))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Xs)
                            .child(
                                lucide(LucideIcon::Undo2)
                                    .size(px(12.))
                                    .text_color(c.foreground),
                            )
                            .label("Change")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.resolve(index, None, cx);
                            })),
                    )
                    .into_any_element()
            } else {
                let pick = |id: &'static str, label: String, resolution: Resolution| {
                    Button::new((id, index))
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Xs)
                        .label(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.resolve(index, Some(resolution.clone()), cx);
                        }))
                };
                let mut buttons = div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(pick(
                        "conflict.ours",
                        format!("Use {ours}"),
                        Resolution::Ours,
                    ))
                    .child(pick(
                        "conflict.theirs",
                        format!("Use {theirs}"),
                        Resolution::Theirs,
                    ))
                    .child(pick(
                        "conflict.both",
                        "Use Both".to_owned(),
                        Resolution::Both,
                    ));
                if let Some(menu) = self.menus.get(index) {
                    buttons = buttons.child(
                        Menu::new(menu, "More ways to resolve")
                            .trigger_style_with(ghost_trigger(theme, ButtonSize::IconXs))
                            .trigger(
                                lucide(LucideIcon::Ellipsis)
                                    .size(px(13.))
                                    .text_color(c.foreground),
                            ),
                    );
                }
                buttons.into_any_element()
            });
        let side = |name: &'static str,
                    label: &str,
                    lines: &[String],
                    color: gpui_kit::Rgba|
         -> gpui_kit::AnyElement {
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(10.))
                        .pt(px(6.))
                        .pb(px(2.))
                        .text_xs()
                        .child(
                            div()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(color)
                                .child(name),
                        )
                        .when(!label.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_color(c.muted_foreground)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(label.to_owned()),
                            )
                        }),
                )
                .child(code_block(lines, color, color.opacity(0.08).into(), theme))
                .into_any_element()
        };
        let body = match &resolution {
            Some(resolution) => div()
                .py(px(4.))
                .child(code_block(
                    &conflict.resolved(resolution),
                    c.primary,
                    c.primary.opacity(0.05).into(),
                    theme,
                ))
                .into_any_element(),
            None => div()
                .flex()
                .flex_col()
                .pb(px(6.))
                .child(side(ours, &conflict.ours_label, &conflict.ours, ours_color))
                .when_some(conflict.base.as_ref(), |this, base| {
                    this.child(side(
                        "Original",
                        &conflict.base_label,
                        base,
                        c.muted_foreground,
                    ))
                })
                .child(side(
                    theirs,
                    &conflict.theirs_label,
                    &conflict.theirs,
                    theirs_color,
                ))
                .into_any_element(),
        };
        div()
            .id(("conflict.region", index))
            .flex_shrink_0()
            .mx(px(12.))
            .my(px(6.))
            .rounded(px(8.))
            .border_1()
            .border_color(if resolution.is_some() {
                c.border
            } else {
                c.warning.opacity(0.6)
            })
            .overflow_hidden()
            .child(header)
            .child(body)
    }
}

impl Render for ConflictView {
    #[allow(clippy::too_many_lines)]
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let operation = self.operation(cx);
        let ours = side_name(Side::Ours, operation);
        let theirs = side_name(Side::Theirs, operation);
        let path = self
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let conflicts = self.conflicts();
        let resolved = self.resolved();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .h(px(44.))
            .px(px(14.))
            .border_b_1()
            .border_color(c.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .child(
                        lucide(LucideIcon::GitMerge)
                            .size(px(15.))
                            .text_color(c.warning),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(path),
                    )
                    .when(conflicts > 0, |this| {
                        this.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(format!("{resolved} of {conflicts} resolved")),
                        )
                    }),
            );
        let body = match &self.status {
            Status::Idle | Status::Loading => div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Reading the conflicts…")
                .into_any_element(),
            Status::Failed(error) => self.render_whole_file(Some(error.clone()), &theme, cx),
            Status::Ready(_) if conflicts == 0 => self.render_whole_file(None, &theme, cx),
            Status::Ready(segments) => {
                let segments = segments.clone();
                let mut list = div()
                    .id("conflict.list")
                    .size_full()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .py(px(6.));
                let mut index = 0;
                let final_segment = segments.len().saturating_sub(1);
                for (i, segment) in segments.iter().enumerate() {
                    match segment {
                        Segment::Common(lines) => {
                            list = list.child(Self::render_common(
                                lines,
                                i == 0,
                                i == final_segment,
                                i,
                                &theme,
                            ));
                        }
                        Segment::Conflict(conflict) => {
                            list = list.child(
                                self.render_conflict(index, conflict, ours, theirs, &theme, cx),
                            );
                            index += 1;
                        }
                    }
                }
                list.into_any_element()
            }
        };
        let footer = (conflicts > 0).then(|| {
            let done = resolved == conflicts;
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .px(px(14.))
                .py(px(8.))
                .border_t_1()
                .border_color(c.border)
                .child(
                    div()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(if done {
                            "Every conflict has a choice. Save to mark the file resolved."
                                .to_owned()
                        } else {
                            let left = conflicts - resolved;
                            format!(
                                "{left} conflict{} left. Saving keeps their markers.",
                                if left == 1 { "" } else { "s" }
                            )
                        }),
                )
                .child(
                    Button::new("conflict.save")
                        .variant(if done {
                            ButtonVariant::Default
                        } else {
                            ButtonVariant::Outline
                        })
                        .size(ButtonSize::Sm)
                        .disabled(resolved == 0)
                        .child(lucide(LucideIcon::Check).size(px(14.)).text_color(if done {
                            c.primary_foreground
                        } else {
                            c.foreground
                        }))
                        .label(if done {
                            "Save and Mark Resolved"
                        } else {
                            "Save Progress"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                )
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .children(footer)
    }
}

impl ConflictView {
    /// For a conflict about the whole file, such as one side deleting it,
    /// or a file with no markers left: pick a side or mark it resolved.
    fn render_whole_file(
        &self,
        error: Option<SharedString>,
        theme: &UiTheme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let c = &theme.colors;
        let operation = self.operation(cx);
        let ours = side_name(Side::Ours, operation);
        let theirs = side_name(Side::Theirs, operation);
        let detail = match self.kind {
            Some(ConflictKind::DeletedByUs) => {
                format!("{ours} deleted this file; {theirs} changed it.")
            }
            Some(ConflictKind::DeletedByThem) => {
                format!("{theirs} deleted this file; {ours} changed it.")
            }
            Some(ConflictKind::BothDeleted) => "Both sides deleted this file.".to_owned(),
            Some(ConflictKind::AddedByUs) => format!("Only {ours} added this file."),
            Some(ConflictKind::AddedByThem) => format!("Only {theirs} added this file."),
            _ => match &error {
                Some(error) => error.to_string(),
                None => "No conflict markers are left. Mark the file resolved when it is right."
                    .to_owned(),
            },
        };
        let markers_gone = error.is_none()
            && matches!(
                self.kind,
                Some(ConflictKind::BothModified | ConflictKind::BothAdded)
            );
        let path = self.path.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .p(px(24.))
            .child(
                lucide(LucideIcon::GitMerge)
                    .size(px(24.))
                    .text_color(c.muted_foreground),
            )
            .child(
                div()
                    .max_w(px(360.))
                    .text_center()
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child(detail),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        Button::new("conflict.take-ours")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label(Side::Ours.label(operation))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.take_side(Side::Ours, cx);
                            })),
                    )
                    .child(
                        Button::new("conflict.take-theirs")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label(Side::Theirs.label(operation))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.take_side(Side::Theirs, cx);
                            })),
                    )
                    .when(markers_gone, |this| {
                        this.child(
                            Button::new("conflict.mark-resolved")
                                .variant(ButtonVariant::Default)
                                .size(ButtonSize::Sm)
                                .label("Mark Resolved")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(path) = path.clone() {
                                        this.model.update(cx, |model, cx| {
                                            model.run(
                                                format!("Mark {} resolved", path.display()),
                                                move |repository| repository.mark_resolved(&[path]),
                                                cx,
                                            );
                                        });
                                    }
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
}
