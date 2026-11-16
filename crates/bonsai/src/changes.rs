//! The Changes view: conflicted, staged, unstaged and untracked files, with
//! staging, discarding and ignoring (CONTEXT.md: Change).

use std::path::{Path, PathBuf};

use bonsai_git::{Change, ChangeKind, DiffTarget, EntryStatus, IgnoreRule, Snapshot};
use gpui_icons::LucideIcon;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, ElementId, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled,
    Subscription, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonVariant},
        context_menu::context_menu,
        dialog::{
            DialogHandle, dialog, dialog_description, dialog_footer, dialog_popup, dialog_title,
        },
        menu::{MenuItem, MenuState},
        theme::UiTheme,
        virtual_list::{
            ListItem, ListRow, ListSelectionMode, VirtualList, VirtualListEvent, VirtualListState,
        },
    },
};

/// The groups of the Changes list, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Conflicted,
    Staged,
    Unstaged,
    Untracked,
}

impl Group {
    const ORDER: [Self; 4] = [
        Self::Conflicted,
        Self::Staged,
        Self::Unstaged,
        Self::Untracked,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Conflicted => "Conflicts",
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
            Self::Untracked => "Untracked",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Conflicted => "conflicted",
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Untracked => "untracked",
        }
    }
}

/// One row: a group heading or a change within a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeRow {
    pub id: SharedString,
    pub group: Group,
    /// `None` for a heading.
    pub change: Option<Change>,
    /// Changes in the group, for headings.
    pub count: usize,
}

impl ChangeRow {
    /// The status letter for this row's side of the change.
    #[must_use]
    pub fn status(&self) -> Option<char> {
        let change = self.change.as_ref()?;
        Some(match (self.group, change.kind) {
            (Group::Conflicted, _) => 'U',
            (Group::Untracked, _) => '?',
            (Group::Staged, ChangeKind::Tracked { index: Some(s), .. })
            | (
                Group::Unstaged,
                ChangeKind::Tracked {
                    work_tree: Some(s), ..
                },
            ) => letter(s),
            _ => ' ',
        })
    }
}

fn letter(status: EntryStatus) -> char {
    match status {
        EntryStatus::Modified => 'M',
        EntryStatus::TypeChanged => 'T',
        EntryStatus::Added => 'A',
        EntryStatus::Deleted => 'D',
        EntryStatus::Renamed => 'R',
        EntryStatus::Copied => 'C',
    }
}

/// Groups `changes` into heading and change rows. A file with both staged and
/// unstaged changes appears in both groups; empty groups are left out.
#[must_use]
pub fn group_rows(changes: &[Change]) -> Vec<ChangeRow> {
    let mut rows = Vec::with_capacity(changes.len() + 4);
    for group in Group::ORDER {
        let members: Vec<&Change> = changes
            .iter()
            .filter(|change| match (group, change.kind) {
                (Group::Conflicted, ChangeKind::Conflicted(_))
                | (Group::Untracked, ChangeKind::Untracked) => true,
                (Group::Staged, _) => change.is_staged(),
                (Group::Unstaged, _) => change.is_unstaged(),
                _ => false,
            })
            .collect();
        if members.is_empty() {
            continue;
        }
        rows.push(ChangeRow {
            id: format!("{}:", group.key()).into(),
            group,
            change: None,
            count: members.len(),
        });
        for change in members {
            rows.push(ChangeRow {
                id: format!("{}:{}", group.key(), change.path.to_string_lossy()).into(),
                group,
                change: Some(change.clone()),
                count: 0,
            });
        }
    }
    rows
}

/// What a selection of rows can do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Targets {
    /// Paths in the Staged group.
    pub staged: Vec<PathBuf>,
    /// Paths with unstaged changes to tracked files.
    pub unstaged: Vec<PathBuf>,
    pub untracked: Vec<PathBuf>,
    pub conflicted: Vec<PathBuf>,
}

impl Targets {
    #[must_use]
    pub fn from_rows<'a>(rows: impl IntoIterator<Item = &'a ChangeRow>) -> Self {
        let mut targets = Self::default();
        for row in rows {
            let Some(change) = &row.change else { continue };
            let list = match row.group {
                Group::Staged => &mut targets.staged,
                Group::Unstaged => &mut targets.unstaged,
                Group::Untracked => &mut targets.untracked,
                Group::Conflicted => &mut targets.conflicted,
            };
            if !list.contains(&change.path) {
                list.push(change.path.clone());
            }
        }
        targets
    }

    /// Paths that staging would add to the index.
    #[must_use]
    pub fn stageable(&self) -> Vec<PathBuf> {
        let mut paths = self.unstaged.clone();
        paths.extend(self.untracked.iter().cloned());
        paths.extend(self.conflicted.iter().cloned());
        paths
    }

    /// Whether anything can be discarded (unstaged or untracked).
    #[must_use]
    pub fn discardable(&self) -> usize {
        self.unstaged.len() + self.untracked.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty()
            && self.unstaged.is_empty()
            && self.untracked.is_empty()
            && self.conflicted.is_empty()
    }

    /// The one selected path, when exactly one file is selected.
    fn single(&self) -> Option<&Path> {
        let mut all = self
            .staged
            .iter()
            .chain(&self.unstaged)
            .chain(&self.untracked)
            .chain(&self.conflicted);
        let first = all.next()?;
        all.all(|p| p == first).then_some(first.as_path())
    }
}

/// What the Changes view tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangesEvent {
    /// The active row changed: show this diff, titled by its path.
    Focused(Option<DiffTarget>, SharedString),
}

impl ChangeRow {
    /// The diff that shows this row's side of the change.
    #[must_use]
    pub fn diff_target(&self) -> Option<DiffTarget> {
        let path = self.change.as_ref()?.path.clone();
        Some(match self.group {
            Group::Staged => DiffTarget::Staged(vec![path]),
            Group::Unstaged | Group::Conflicted => DiffTarget::Unstaged(vec![path]),
            Group::Untracked => DiffTarget::Untracked(path),
        })
    }
}

pub struct ChangesView {
    model: Entity<RepositoryModel>,
    rows: Vec<ChangeRow>,
    list: VirtualListState,
    menu: Entity<MenuState>,
    discard: DialogHandle,
    /// What the open Discard dialog will throw away.
    pending_discard: Targets,
    _events: Subscription,
}

impl EventEmitter<ChangesEvent> for ChangesView {}

impl ChangesView {
    /// The row the list's focus is on, if it is a change.
    #[must_use]
    pub fn focused_row(&self) -> Option<&ChangeRow> {
        let focused = self.list.focused()?;
        self.rows
            .iter()
            .find(|row| matches!(&focused, ElementId::Name(name) if *name == row.id))
            .filter(|row| row.change.is_some())
    }

    fn emit_focus(&mut self, cx: &mut Context<Self>) {
        let (target, title) = self
            .focused_row()
            .map_or((None, SharedString::default()), |row| {
                let path = row
                    .change
                    .as_ref()
                    .map(|c| c.path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (
                    row.diff_target(),
                    format!("{path} · {}", row.group.title()).into(),
                )
            });
        cx.emit(ChangesEvent::Focused(target, title));
    }

    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let list = VirtualListState::new(Vec::new()).expect("an empty list has no duplicates");
        list.set_selection_mode(ListSelectionMode::Multiple);
        let menu = cx.new(|cx| MenuState::new(Vec::new(), cx));
        let events = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            if matches!(event, RepositoryEvent::SnapshotChanged) {
                this.sync(cx);
            }
        });
        let mut this = Self {
            model,
            rows: Vec::new(),
            list,
            menu,
            discard: DialogHandle::new(false),
            pending_discard: Targets::default(),
            _events: events,
        };
        this.sync(cx);
        this
    }

    #[must_use]
    pub fn rows(&self) -> &[ChangeRow] {
        &self.rows
    }

    #[must_use]
    pub fn list(&self) -> &VirtualListState {
        &self.list
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let rows = self
            .model
            .read(cx)
            .snapshot()
            .map(|snapshot: &Snapshot| group_rows(&snapshot.changes))
            .unwrap_or_default();
        let items = rows
            .iter()
            .map(|row| {
                let label = match &row.change {
                    None => format!("{} ({})", row.group.title(), row.count),
                    Some(change) => {
                        format!("{} {}", change.path.to_string_lossy(), row.group.title())
                    }
                };
                ListItem::new(ElementId::Name(row.id.clone()), label).disabled(row.change.is_none())
            })
            .collect();
        // IDs are unique by construction: group key plus path.
        let _ = self.list.replace_items(items);
        self.rows = rows;
        self.rebuild_menu(cx);
        self.emit_focus(cx);
        cx.notify();
    }

    /// The selected change rows.
    #[must_use]
    pub fn selection(&self) -> Targets {
        let selected = self.list.selected();
        Targets::from_rows(self.rows.iter().filter(|row| {
            selected
                .iter()
                .any(|id| matches!(id, ElementId::Name(name) if *name == row.id))
        }))
    }

    /// Stages the selection, or unstages it when everything selected is
    /// already staged.
    pub fn toggle_selection(&mut self, cx: &mut Context<Self>) {
        let targets = self.selection();
        let stageable = targets.stageable();
        if !stageable.is_empty() {
            self.stage(stageable, cx);
        } else if !targets.staged.is_empty() {
            self.unstage(targets.staged, cx);
        }
    }

    pub fn stage(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let name = describe("Stage", &paths);
        self.model.update(cx, |model, cx| {
            model.run(name, move |repository| repository.stage(&paths), cx);
        });
    }

    pub fn unstage(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let name = describe("Unstage", &paths);
        self.model.update(cx, |model, cx| {
            model.run(name, move |repository| repository.unstage(&paths), cx);
        });
    }

    pub fn stage_all(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| {
            model.run("Stage all", bonsai_git::Repository::stage_all, cx);
        });
    }

    pub fn unstage_all(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| {
            model.run("Unstage all", bonsai_git::Repository::unstage_all, cx);
        });
    }

    /// Discards without asking; the UI asks first through [`Self::ask_discard`].
    pub fn discard(&mut self, targets: Targets, cx: &mut Context<Self>) {
        if targets.discardable() == 0 {
            return;
        }
        let Targets {
            unstaged,
            untracked,
            ..
        } = targets;
        let name = describe("Discard", &[unstaged.clone(), untracked.clone()].concat());
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| {
                    repository.discard(&unstaged)?;
                    repository.delete_untracked(&untracked)
                },
                cx,
            );
        });
    }

    fn ask_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.selection();
        if targets.discardable() == 0 {
            return;
        }
        self.pending_discard = targets;
        self.discard.open(window, cx);
        cx.notify();
    }

    pub fn ignore(&mut self, rule: IgnoreRule, cx: &mut Context<Self>) {
        let name = format!("Ignore {}", rule.pattern());
        self.model.update(cx, |model, cx| {
            model.run(name, move |repository| repository.ignore(&rule), cx);
        });
    }

    fn absolute(&self, path: &Path, cx: &gpui_kit::App) -> PathBuf {
        self.model.read(cx).repository().root().join(path)
    }

    /// Rebuilds the context menu for the current selection.
    fn rebuild_menu(&mut self, cx: &mut Context<Self>) {
        let targets = self.selection();
        let this = cx.entity().downgrade();
        let action = |this: &WeakEntity<Self>,
                      f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let this = this.clone();
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
                let _ = this.update(cx, |view, cx| f(view, window, cx));
            }
        };
        let mut items = Vec::new();
        let stageable = targets.stageable().len();
        items.push(
            MenuItem::new("changes.stage", "Stage")
                .icon(LucideIcon::Plus)
                .shortcut("Space")
                .disabled(stageable == 0)
                .on_click(action(&this, |view, _, cx| {
                    let paths = view.selection().stageable();
                    view.stage(paths, cx);
                })),
        );
        items.push(
            MenuItem::new("changes.unstage", "Unstage")
                .icon(LucideIcon::Minus)
                .disabled(targets.staged.is_empty())
                .on_click(action(&this, |view, _, cx| {
                    let paths = view.selection().staged;
                    view.unstage(paths, cx);
                })),
        );
        items.push(MenuItem::separator());
        items.push(
            MenuItem::new("changes.discard", "Discard Changes…")
                .icon(LucideIcon::Undo2)
                .disabled(targets.discardable() == 0)
                .on_click(action(&this, Self::ask_discard)),
        );
        if let Some(path) = targets.single() {
            let mut ignore = vec![
                MenuItem::new(
                    "changes.ignore.path",
                    format!("Ignore “{}”", path.to_string_lossy()),
                )
                .on_click({
                    let this = this.clone();
                    let path = path.to_path_buf();
                    move |_, _, cx| {
                        let _ = this.update(cx, |view, cx| {
                            view.ignore(IgnoreRule::Path(path.clone()), cx);
                        });
                    }
                }),
            ];
            if let Some(extension) = path.extension().map(|e| e.to_string_lossy().into_owned()) {
                ignore.push(
                    MenuItem::new(
                        "changes.ignore.extension",
                        format!("Ignore All .{extension} Files"),
                    )
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            let _ = this.update(cx, |view, cx| {
                                view.ignore(IgnoreRule::Extension(extension.clone()), cx);
                            });
                        }
                    }),
                );
            }
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                let parent = parent.to_path_buf();
                ignore.push(
                    MenuItem::new(
                        "changes.ignore.folder",
                        format!("Ignore Folder “{}/”", parent.to_string_lossy()),
                    )
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            let _ = this.update(cx, |view, cx| {
                                view.ignore(IgnoreRule::Folder(parent.clone()), cx);
                            });
                        }
                    }),
                );
            }
            items.push(
                MenuItem::submenu("changes.ignore", "Ignore", ignore).icon(LucideIcon::EyeOff),
            );
            items.push(MenuItem::separator());
            let reveal = path.to_path_buf();
            items.push(
                MenuItem::new("changes.reveal", "Reveal in Finder")
                    .icon(LucideIcon::FolderOpen)
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            if let Ok(path) =
                                this.read_with(cx, |view, cx| view.absolute(&reveal, cx))
                            {
                                cx.reveal_path(&path);
                            }
                        }
                    }),
            );
            let copy = path.to_string_lossy().into_owned();
            items.push(
                MenuItem::new("changes.copy-path", "Copy Path")
                    .icon(LucideIcon::Copy)
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    }),
            );
        }
        self.menu.update(cx, |menu, cx| menu.set_items(items, cx));
    }

    fn render_row(row: &ChangeRow, theme: &UiTheme) -> gpui_kit::AnyElement {
        let c = &theme.colors;
        let Some(change) = &row.change else {
            return div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(c.muted_foreground)
                .child(row.group.title().to_uppercase())
                .child(row.count.to_string())
                .into_any_element();
        };
        let status = row.status().unwrap_or(' ');
        let status_color = match status {
            'A' | '?' => c.success,
            'D' => c.destructive,
            'U' => c.warning,
            'R' | 'C' => c.info,
            _ => c.muted_foreground,
        };
        let path = change.path.to_string_lossy();
        let (folder, name) = match path.rsplit_once('/') {
            Some((folder, name)) => (Some(folder.to_owned()), name.to_owned()),
            None => (None, path.into_owned()),
        };
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .w(px(14.))
                    .flex_shrink_0()
                    .font_family(theme.fonts.mono.clone())
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(status_color)
                    .child(status.to_string()),
            )
            .child(div().flex_shrink_0().child(name))
            .when_some(change.original_path.as_ref(), |this, original| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!("← {}", original.to_string_lossy())),
                )
            })
            .when_some(folder, |this, folder| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(folder),
                )
            })
            .into_any_element()
    }

    fn render_discard(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let description = discard_description(&self.pending_discard);
        let popup = dialog_popup("changes.discard.popup", "Discard Changes", cx)
            .child(dialog_title("changes.discard.title", cx).child("Discard changes?"))
            .child(
                dialog_description("changes.discard.description", cx)
                    .child(format!("{description} This can't be undone.")),
            )
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("changes.discard.cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.discard.close(window, cx);
                            })),
                    )
                    .child(
                        Button::new("changes.discard.confirm")
                            .variant(ButtonVariant::Destructive)
                            .label("Discard")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.discard.close(window, cx);
                                let targets = std::mem::take(&mut this.pending_discard);
                                this.discard(targets, cx);
                            })),
                    ),
            );
        dialog("changes.discard", &self.discard, popup, window, cx)
    }
}

/// What the Discard dialog says will happen.
fn discard_description(targets: &Targets) -> String {
    let files = |n: usize| if n == 1 { "file" } else { "files" };
    let mut parts = Vec::new();
    if !targets.unstaged.is_empty() {
        let n = targets.unstaged.len();
        parts.push(format!(
            "{n} {} will go back to the last staged or committed version",
            files(n)
        ));
    }
    if !targets.untracked.is_empty() {
        let n = targets.untracked.len();
        parts.push(format!("{n} untracked {} will be deleted", files(n)));
    }
    format!("{}.", parts.join(", and "))
}

/// An operation name for the activity log, such as `Stage src/main.rs` or
/// `Stage 12 files`.
fn describe(verb: &str, paths: &[PathBuf]) -> String {
    match paths {
        [one] => format!("{verb} {}", one.to_string_lossy()),
        many => format!("{verb} {} files", many.len()),
    }
}

impl Render for ChangesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let staged = self
            .rows
            .iter()
            .filter(|r| r.group == Group::Staged && r.change.is_some())
            .count();
        let total = self
            .model
            .read(cx)
            .snapshot()
            .map_or(0, |s| s.changes.len());
        let rows = self.rows.clone();
        let row_theme = theme.clone();
        let view = cx.entity().downgrade();
        let list = VirtualList::new(
            "changes.list",
            "Changes",
            self.list.clone(),
            move |row: &ListRow, _, _| {
                rows.get(row.index).map_or_else(
                    || div().into_any_element(),
                    |r| Self::render_row(r, &row_theme),
                )
            },
        )
        .empty(
            div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child("No changes. The work tree matches the last commit."),
        )
        .on_event(move |event, _, cx| {
            let _ = view.update(cx, |view, cx| match event {
                VirtualListEvent::SelectionChanged | VirtualListEvent::FocusChanged => {
                    view.rebuild_menu(cx);
                    view.emit_focus(cx);
                    cx.notify();
                }
                VirtualListEvent::Activate(_) => view.toggle_selection(cx),
            });
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(c.border)
                    .child(
                        div()
                            .text_sm()
                            .text_color(c.muted_foreground)
                            .child(format!("{total} changed · {staged} staged")),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .child(
                                Button::new("changes.unstage-all")
                                    .variant(ButtonVariant::Ghost)
                                    .label("Unstage All")
                                    .disabled(staged == 0)
                                    .on_click(cx.listener(|this, _, _, cx| this.unstage_all(cx))),
                            )
                            .child(
                                Button::new("changes.stage-all")
                                    .variant(ButtonVariant::Outline)
                                    .label("Stage All")
                                    .disabled(total == staged)
                                    .on_click(cx.listener(|this, _, _, cx| this.stage_all(cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .id("changes.body")
                    .flex_1()
                    .min_h_0()
                    .p(px(4.))
                    .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                        let keys = &event.keystroke;
                        if keys.key == "space" && !keys.modifiers.modified() {
                            this.toggle_selection(cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(context_menu(&self.menu, "Change actions", list).size_full()),
            )
            .child(self.render_discard(window, cx))
            .child(
                div()
                    .px(px(12.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(c.border)
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child("Space stages or unstages the selection. Right-click for more."),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str, kind: ChangeKind) -> Change {
        Change {
            path: path.into(),
            original_path: None,
            kind,
            submodule: false,
        }
    }

    #[test]
    fn groups_rows_and_lists_partly_staged_files_twice() {
        let changes = [
            change(
                "both.rs",
                ChangeKind::Tracked {
                    index: Some(EntryStatus::Modified),
                    work_tree: Some(EntryStatus::Modified),
                },
            ),
            change("new.txt", ChangeKind::Untracked),
            change(
                "gone.rs",
                ChangeKind::Tracked {
                    index: None,
                    work_tree: Some(EntryStatus::Deleted),
                },
            ),
            change(
                "clash.rs",
                ChangeKind::Conflicted(bonsai_git::ConflictKind::BothModified),
            ),
        ];
        let rows = group_rows(&changes);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_ref()).collect();
        assert_eq!(
            ids,
            [
                "conflicted:",
                "conflicted:clash.rs",
                "staged:",
                "staged:both.rs",
                "unstaged:",
                "unstaged:both.rs",
                "unstaged:gone.rs",
                "untracked:",
                "untracked:new.txt",
            ]
        );
        assert_eq!(rows[4].count, 2);
        assert_eq!(rows[6].status(), Some('D'));
        assert_eq!(rows[8].status(), Some('?'));

        let all = Targets::from_rows(&rows);
        assert_eq!(all.staged, [PathBuf::from("both.rs")]);
        assert_eq!(all.stageable().len(), 4);
        assert_eq!(all.discardable(), 3);
        assert_eq!(
            Targets::from_rows(&rows[3..4]).single(),
            Some(Path::new("both.rs"))
        );
        assert_eq!(Targets::from_rows(&rows[3..7]).single(), None);
    }

    #[test]
    fn discard_dialog_says_what_happens() {
        let targets = Targets {
            unstaged: vec!["a".into(), "b".into()],
            untracked: vec!["c".into()],
            ..Targets::default()
        };
        assert_eq!(
            discard_description(&targets),
            "2 files will go back to the last staged or committed version, and 1 untracked file will be deleted."
        );
    }

    #[test]
    fn describes_operations_for_the_activity_log() {
        assert_eq!(describe("Stage", &["a.rs".into()]), "Stage a.rs");
        assert_eq!(
            describe("Stage", &["a".into(), "b".into()]),
            "Stage 2 files"
        );
    }

    mod view {
        use bonsai_git::{Git, Repository};
        use gpui_kit::TestAppContext;

        use super::super::*;

        #[gpui_kit::test]
        fn toggling_stages_and_unstages_the_selection(cx: &mut TestAppContext) {
            cx.update(crate::ui::theme::init);
            let dir = tempfile::tempdir().unwrap();
            let git = Git::locate(None).unwrap();
            let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
            std::fs::write(dir.path().join("a.txt"), "a").unwrap();
            std::fs::write(dir.path().join("b.txt"), "b").unwrap();
            let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
            let view = cx.new(|cx| ChangesView::new(model.clone(), cx));
            cx.run_until_parked();

            let ids = |view: &ChangesView| {
                view.rows()
                    .iter()
                    .map(|r| r.id.to_string())
                    .collect::<Vec<_>>()
            };
            view.read_with(cx, |view, _| {
                assert_eq!(
                    ids(view),
                    ["untracked:", "untracked:a.txt", "untracked:b.txt"]
                );
            });

            view.update(cx, |view, cx| {
                view.list()
                    .set_selected([ElementId::Name("untracked:a.txt".into())]);
                view.toggle_selection(cx);
            });
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(
                    ids(view),
                    ["staged:", "staged:a.txt", "untracked:", "untracked:b.txt"]
                );
                // The staged row is a new ID, so the selection is empty again.
                assert!(view.selection().is_empty());
            });

            view.update(cx, |view, cx| {
                view.list()
                    .set_selected([ElementId::Name("staged:a.txt".into())]);
                view.toggle_selection(cx);
            });
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(view.rows().len(), 3);
            });

            view.update(cx, |view, cx| {
                view.list()
                    .set_selected([ElementId::Name("untracked:b.txt".into())]);
                let targets = view.selection();
                view.discard(targets, cx);
            });
            cx.run_until_parked();
            assert!(!dir.path().join("b.txt").exists());
        }
    }
}
