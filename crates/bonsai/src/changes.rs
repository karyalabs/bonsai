//! The Changes view: conflicted, staged, unstaged and untracked files, with
//! staging, discarding and ignoring (CONTEXT.md: Change).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
};

use bonsai_git::{
    Change, ChangeKind, DiffTarget, EntryStatus, IgnoreRule, InProgress, LineCount, LineCounts,
    OperationAction, Side, Snapshot,
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, ElementId, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Task, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    composer::CommitComposer,
    confirm::{self, Confirm},
    file_icon::file_icon,
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        context_menu::context_menu,
        dialog::{DialogHandle, dialog},
        input::{Input, InputEvent, InputState},
        menu::{MenuItem, MenuState},
        theme::UiTheme,
        toggle_group::{ToggleGroup, ToggleGroupItem},
        tooltip::text_tooltip,
        tree::{Tree, TreeEvent, TreeNode, TreeRow, TreeRowKind, TreeState},
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

/// The Changes tree: a folder per group, then the folders of each path.
/// Leaves keep their row's id; a folder is `group:dir/`, so a folder id is a
/// prefix of every row under it.
#[must_use]
pub fn tree_nodes(rows: &[ChangeRow]) -> Vec<TreeNode> {
    #[derive(Default)]
    struct Dir<'a> {
        dirs: BTreeMap<&'a str, Dir<'a>>,
        files: Vec<(&'a str, &'a ChangeRow)>,
    }
    fn nodes(prefix: &str, dir: Dir<'_>) -> Vec<TreeNode> {
        let mut out: Vec<TreeNode> = dir
            .dirs
            .into_iter()
            .map(|(name, child)| {
                let id = format!("{prefix}{name}/");
                let children = nodes(&id, child);
                TreeNode::folder(id, name.to_owned(), children)
            })
            .collect();
        out.extend(
            dir.files
                .into_iter()
                .map(|(name, row)| TreeNode::leaf(row.id.clone(), name.to_owned())),
        );
        out
    }
    let mut groups = Vec::new();
    for heading in rows.iter().filter(|row| row.change.is_none()) {
        let mut root = Dir::default();
        let paths: Vec<(String, &ChangeRow)> = rows
            .iter()
            .filter(|row| row.group == heading.group)
            .filter_map(|row| {
                let path = row.change.as_ref()?.path.to_string_lossy().into_owned();
                Some((path, row))
            })
            .collect();
        for (path, row) in &paths {
            let mut parts: Vec<&str> = path.split('/').collect();
            let name = parts.pop().unwrap_or_default();
            let mut dir = &mut root;
            for part in parts {
                dir = dir.dirs.entry(part).or_default();
            }
            dir.files.push((name, row));
        }
        let children = nodes(&heading.id, root);
        groups.push(TreeNode::folder(
            heading.id.clone(),
            format!("{} ({})", heading.group.title(), heading.count),
            children,
        ));
    }
    groups
}

/// Whether the row with `row_id` is `selected_id` or sits in that folder.
fn covers(selected_id: &str, row_id: &str) -> bool {
    selected_id == row_id
        || ((selected_id.ends_with('/') || selected_id.ends_with(':'))
            && row_id.starts_with(selected_id))
}

/// Every folder id in `nodes`.
fn folder_ids(nodes: &[TreeNode], out: &mut Vec<SharedString>) {
    for node in nodes {
        if let crate::ui::tree::TreeChildren::Loaded(children) = &node.children {
            out.push(node.id.clone());
            folder_ids(children, out);
        }
    }
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
    /// Purge from History was chosen for this path.
    Purge(PathBuf),
    /// The active row is this conflicted file: show the resolver.
    Conflict(PathBuf, bonsai_git::ConflictKind),
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
    tree: TreeState,
    /// Shows folders (gpuicn Tree) instead of a flat list per group.
    tree_mode: bool,
    /// Folders the person closed; every other folder opens.
    collapsed: HashSet<SharedString>,
    menu: Entity<MenuState>,
    discard: DialogHandle,
    /// What the open Discard dialog will throw away.
    pending_discard: Targets,
    /// Built on first render, which has the window its inputs need.
    composer: Option<Entity<CommitComposer>>,
    /// Lines added and removed per file, read after each snapshot.
    counts: Rc<LineCounts>,
    counts_task: Option<Task<()>>,
    /// The filter field, built on first render.
    filter: Option<(Entity<InputState>, Subscription)>,
    /// Shows only paths containing this text (case-insensitive).
    filter_text: String,
    _events: Subscription,
}

/// More changes than this skip line counts, which would cost a second
/// full diff of the work tree.
const MAX_COUNTED_CHANGES: usize = 5_000;

/// What a row's or heading's hover button does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowAction {
    Stage(Vec<PathBuf>),
    Unstage(Vec<PathBuf>),
    Discard(Targets),
    StageGroup(Group),
    UnstageAll,
    DiscardGroup(Group),
}

/// The line counts for a row's side of its change.
fn row_count(counts: &LineCounts, row: &ChangeRow) -> Option<LineCount> {
    let path = &row.change.as_ref()?.path;
    match row.group {
        Group::Staged => counts.staged.get(path).copied(),
        Group::Unstaged | Group::Conflicted => counts.unstaged.get(path).copied(),
        Group::Untracked => None,
    }
}

impl EventEmitter<ChangesEvent> for ChangesView {}

impl ChangesView {
    /// The row the list's focus is on, if it is a change.
    #[must_use]
    pub fn focused_row(&self) -> Option<&ChangeRow> {
        let focused = if self.tree_mode {
            // Before any click the tree has a selection but no focus.
            self.tree
                .focused()
                .or_else(|| self.tree.selected().into_iter().next())?
        } else {
            match self.list.focused()? {
                ElementId::Name(name) => name,
                _ => return None,
            }
        };
        self.rows
            .iter()
            .find(|row| row.id == focused)
            .filter(|row| row.change.is_some())
    }

    #[must_use]
    pub fn tree(&self) -> &TreeState {
        &self.tree
    }

    #[must_use]
    pub fn tree_mode(&self) -> bool {
        self.tree_mode
    }

    /// Switches between the flat list and the folder tree, keeping what is
    /// selected, and remembers the choice in Settings.
    pub fn set_tree_mode(&mut self, tree_mode: bool, cx: &mut Context<Self>) {
        if self.tree_mode == tree_mode {
            return;
        }
        let selected: Vec<SharedString> = self.selected_rows().map(|row| row.id.clone()).collect();
        self.tree_mode = tree_mode;
        if tree_mode {
            self.tree.set_selected(selected);
        } else {
            self.list
                .set_selected(selected.into_iter().map(ElementId::Name));
        }
        let _ = crate::settings::update(cx, |s| s.changes_tree = tree_mode);
        self.rebuild_menu(cx);
        self.emit_focus(cx);
        cx.notify();
    }

    /// Shows the focused change in the diff pane again, such as after the
    /// History view had it.
    pub fn refocus(&mut self, cx: &mut Context<Self>) {
        self.emit_focus(cx);
    }

    fn emit_focus(&mut self, cx: &mut Context<Self>) {
        if let Some((path, kind)) = self.focused_row().and_then(|row| {
            let change = row.change.as_ref()?;
            match (row.group, change.kind) {
                (Group::Conflicted, ChangeKind::Conflicted(kind)) => {
                    Some((change.path.clone(), kind))
                }
                _ => None,
            }
        }) {
            cx.emit(ChangesEvent::Conflict(path, kind));
            return;
        }
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
        let tree = TreeState::new(Vec::new(), []).expect("an empty tree has no duplicates");
        tree.set_selection_mode(ListSelectionMode::Multiple);
        let tree_mode = crate::settings::get(cx).changes_tree;
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
            tree,
            tree_mode,
            collapsed: HashSet::new(),
            menu,
            discard: DialogHandle::new(false),
            pending_discard: Targets::default(),
            composer: None,
            counts: Rc::default(),
            counts_task: None,
            filter: None,
            filter_text: String::new(),
            _events: events,
        };
        this.sync(cx);
        this
    }

    /// Commits with the composer's message, as Cmd+Enter does.
    pub fn commit(&mut self, cx: &mut Context<Self>) {
        if let Some(composer) = &self.composer {
            composer.update(cx, CommitComposer::commit);
        }
    }

    /// Commits, then pushes, as Shift+Cmd+Enter does.
    pub fn commit_and_push(&mut self, cx: &mut Context<Self>) {
        if let Some(composer) = &self.composer {
            composer.update(cx, CommitComposer::commit_and_push);
        }
    }

    #[must_use]
    pub fn rows(&self) -> &[ChangeRow] {
        &self.rows
    }

    #[must_use]
    pub fn list(&self) -> &VirtualListState {
        &self.list
    }

    fn ensure_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((input, _)) = &self.filter {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter changed files"));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                this.set_filter(&text, cx);
            }
        });
        self.filter = Some((input.clone(), subscription));
        input
    }

    /// Line counts for this row's side of the change.
    #[must_use]
    pub fn line_count(&self, row: &ChangeRow) -> Option<LineCount> {
        row_count(&self.counts, row)
    }

    /// Shows only the paths that contain `text`.
    pub fn set_filter(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.filter_text != text {
            text.clone_into(&mut self.filter_text);
            self.sync(cx);
        }
    }

    fn load_counts(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let Some(snapshot) = model.snapshot() else {
            return;
        };
        let tracked = snapshot
            .changes
            .iter()
            .filter(|c| !matches!(c.kind, ChangeKind::Untracked))
            .count();
        if tracked == 0 || snapshot.changes.len() > MAX_COUNTED_CHANGES {
            self.counts = Rc::default();
            return;
        }
        let repository = model.repository().clone();
        let task = cx.background_spawn(async move { repository.line_counts() });
        self.counts_task = Some(cx.spawn(async move |this, cx| {
            let Ok(counts) = task.await else { return };
            let _ = this.update(cx, |this, cx| {
                this.counts = Rc::new(counts);
                cx.notify();
            });
        }));
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let filter = self.filter_text.trim().to_lowercase();
        let rows = self
            .model
            .read(cx)
            .snapshot()
            .map(|snapshot: &Snapshot| {
                if filter.is_empty() {
                    group_rows(&snapshot.changes)
                } else {
                    let matching: Vec<Change> = snapshot
                        .changes
                        .iter()
                        .filter(|c| c.path.to_string_lossy().to_lowercase().contains(&filter))
                        .cloned()
                        .collect();
                    group_rows(&matching)
                }
            })
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
        let nodes = tree_nodes(&rows);
        let mut folders = Vec::new();
        folder_ids(&nodes, &mut folders);
        // Folder ids end in '/' or ':', which no path does, so they are unique.
        let _ = self.tree.replace_nodes(nodes);
        self.tree.expand_folders(
            folders
                .into_iter()
                .filter(|folder| !self.collapsed.contains(folder)),
        );
        // Nothing focused yet: show the first change, so the diff pane
        // never sits empty while there is something to look at.
        if let Some(first) = rows.iter().find(|row| row.change.is_some()) {
            if self.tree_mode {
                if self.tree.focused().is_none() && self.tree.selected().is_empty() {
                    self.tree.set_selected([first.id.clone()]);
                }
            } else if self.list.focused().is_none() {
                self.list.select(
                    &ElementId::Name(first.id.clone()),
                    crate::ui::virtual_list::ListSelectionGesture::Replace,
                );
            }
        }
        self.rows = rows;
        self.load_counts(cx);
        self.rebuild_menu(cx);
        self.emit_focus(cx);
        cx.notify();
    }

    /// The selected change rows.
    #[must_use]
    pub fn selection(&self) -> Targets {
        Targets::from_rows(self.selected_rows())
    }

    /// The selected change rows; in the tree, a selected folder stands for
    /// every change under it.
    fn selected_rows(&self) -> impl Iterator<Item = &ChangeRow> {
        let selected: Vec<SharedString> = if self.tree_mode {
            self.tree.selected()
        } else {
            self.list
                .selected()
                .into_iter()
                .filter_map(|id| match id {
                    ElementId::Name(name) => Some(name),
                    _ => None,
                })
                .collect()
        };
        self.rows.iter().filter(move |row| {
            row.change.is_some() && selected.iter().any(|id| covers(id, &row.id))
        })
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
        self.ask_discard_targets(targets, window, cx);
    }

    /// Asks before discarding `targets`.
    pub fn ask_discard_targets(
        &mut self,
        targets: Targets,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn resolve_with(&mut self, paths: Vec<PathBuf>, side: Side, cx: &mut Context<Self>) {
        let name = describe(
            if side == Side::Ours {
                "Keep our version of"
            } else {
                "Keep their version of"
            },
            &paths,
        );
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.resolve_with(&paths, side),
                cx,
            );
        });
    }

    /// Continues, skips or aborts the merge, rebase or pick in progress.
    pub fn resolve_operation(&mut self, action: OperationAction, cx: &mut Context<Self>) {
        let name = match action {
            OperationAction::Continue => "Continue",
            OperationAction::Skip => "Skip",
            OperationAction::Abort => "Abort",
        };
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.resolve_operation(action).map(|_| ()),
                cx,
            );
        });
    }

    fn render_operation(
        &self,
        operation: InProgress,
        conflicts: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let name = match operation {
            InProgress::Merge => "Merge",
            InProgress::Rebase => "Rebase",
            InProgress::ApplyMailbox => "Applying patches",
            InProgress::CherryPick => "Cherry-pick",
            InProgress::Revert => "Revert",
            InProgress::Bisect => "Bisect",
        };
        let detail = if conflicts > 0 {
            format!(
                "{name} stopped: resolve {conflicts} conflicted file{}, then continue.",
                if conflicts == 1 { "" } else { "s" }
            )
        } else if operation == InProgress::Bisect {
            "Bisect in progress.".to_owned()
        } else {
            format!("{name} in progress. Every conflict is resolved.")
        };
        let mut buttons = div().flex().gap(px(4.));
        for (action, label, id) in [
            (OperationAction::Abort, "Abort", "changes.op.abort"),
            (OperationAction::Skip, "Skip", "changes.op.skip"),
            (OperationAction::Continue, "Continue", "changes.op.continue"),
        ] {
            if !action.applies_to(operation) {
                continue;
            }
            buttons = buttons.child(
                Button::new(id)
                    .variant(if action == OperationAction::Continue {
                        ButtonVariant::Default
                    } else {
                        ButtonVariant::Outline
                    })
                    .label(label)
                    .disabled(action == OperationAction::Continue && conflicts > 0)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.resolve_operation(action, cx)),
                    ),
            );
        }
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .bg(c.muted)
            .border_b_1()
            .border_color(c.border)
            .child(div().text_sm().child(detail))
            .child(buttons)
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
        if !targets.conflicted.is_empty() {
            let operation = self.model.read(cx).snapshot().and_then(|s| s.operation);
            for side in [Side::Ours, Side::Theirs] {
                let paths = targets.conflicted.clone();
                items.push(
                    MenuItem::new(
                        if side == Side::Ours {
                            "changes.ours"
                        } else {
                            "changes.theirs"
                        },
                        side.label(operation),
                    )
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            let paths = paths.clone();
                            let _ = this.update(cx, |view, cx| view.resolve_with(paths, side, cx));
                        }
                    }),
                );
            }
            let paths = targets.conflicted.clone();
            items.push(
                MenuItem::new("changes.mark-resolved", "Mark Resolved")
                    .icon(LucideIcon::Check)
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            let paths = paths.clone();
                            let _ = this.update(cx, |view, cx| view.stage(paths, cx));
                        }
                    }),
            );
            if let [path] = targets.conflicted.as_slice() {
                let path = path.clone();
                items.push(
                    MenuItem::new("changes.open-editor", "Open in Editor")
                        .icon(LucideIcon::SquarePen)
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| {
                                if let Ok(path) =
                                    this.read_with(cx, |view, cx| view.absolute(&path, cx))
                                {
                                    crate::settings::open_in_editor(&path, cx);
                                }
                            }
                        }),
                );
            }
            items.push(MenuItem::separator());
        }
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
            let purge = path.to_path_buf();
            items.push(
                MenuItem::new("changes.purge", "Purge from History…")
                    .icon(LucideIcon::Eraser)
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            let _ = this.update(cx, |_, cx| {
                                cx.emit(ChangesEvent::Purge(purge.clone()));
                            });
                        }
                    }),
            );
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

    /// `show_folder` adds the file's folder after its name, for the flat list.
    /// The paths of every change row in `group`.
    fn group_paths(&self, group: Group) -> Vec<PathBuf> {
        self.rows
            .iter()
            .filter(|row| row.group == group)
            .filter_map(|row| row.change.as_ref().map(|c| c.path.clone()))
            .collect()
    }

    /// Runs a row or heading action: stage, unstage or discard.
    fn row_action(&mut self, action: RowAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            RowAction::Stage(paths) => self.stage(paths, cx),
            RowAction::Unstage(paths) => self.unstage(paths, cx),
            RowAction::Discard(targets) => self.ask_discard_targets(targets, window, cx),
            RowAction::StageGroup(group) => {
                let paths = self.group_paths(group);
                if !paths.is_empty() {
                    self.stage(paths, cx);
                }
            }
            RowAction::UnstageAll => self.unstage_all(cx),
            RowAction::DiscardGroup(group) => {
                let targets = Targets::from_rows(self.rows.iter().filter(|r| r.group == group));
                self.ask_discard_targets(targets, window, cx);
            }
        }
    }

    fn render_row(
        row: &ChangeRow,
        theme: &UiTheme,
        show_folder: bool,
        count: Option<LineCount>,
        view: &WeakEntity<Self>,
    ) -> gpui_kit::AnyElement {
        let c = &theme.colors;
        let action_button =
            |id: SharedString, icon: LucideIcon, tip: &'static str, action: RowAction| {
                let view = view.clone();
                let theme = theme.clone();
                Button::new(id)
                    .aria_label(tip)
                    .style_with(move |button| {
                        crate::ui::button::style_button(
                            button,
                            false,
                            ButtonVariant::Ghost,
                            ButtonSize::IconXs,
                            &theme,
                        )
                        .tooltip(move |_, cx| text_tooltip(tip.into(), cx))
                    })
                    .child(lucide(icon).size(px(14.)).text_color(c.foreground))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let action = action.clone();
                        let _ = view.update(cx, |view, cx| view.row_action(action, window, cx));
                    })
            };
        let Some(change) = &row.change else {
            let group = row.group;
            let actions = div()
                .flex()
                .items_center()
                .gap(px(2.))
                .opacity(0.)
                .group_hover("change-row", |s| s.opacity(1.))
                .map(|this| match group {
                    Group::Staged => this.child(action_button(
                        "changes.heading.unstage".into(),
                        LucideIcon::Minus,
                        "Unstage All",
                        RowAction::UnstageAll,
                    )),
                    Group::Unstaged | Group::Untracked => this
                        .child(action_button(
                            format!("changes.heading.discard.{}", group.key()).into(),
                            LucideIcon::Undo2,
                            "Discard All…",
                            RowAction::DiscardGroup(group),
                        ))
                        .child(action_button(
                            format!("changes.heading.stage.{}", group.key()).into(),
                            LucideIcon::Plus,
                            "Stage All",
                            RowAction::StageGroup(group),
                        )),
                    Group::Conflicted => this,
                });
            return div()
                .id(SharedString::from(format!(
                    "changes.heading.{}",
                    group.key()
                )))
                .group("change-row")
                .w_full()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(c.muted_foreground)
                .child(row.group.title().to_uppercase())
                .child(
                    div()
                        .px(px(6.))
                        .rounded(px(8.))
                        .bg(c.muted)
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(row.count.to_string()),
                )
                .child(div().flex_1())
                .child(actions)
                .into_any_element();
        };
        let status = row.status().unwrap_or(' ');
        let status_color = match status {
            'A' | '?' => c.success,
            'D' => c.destructive,
            'R' | 'C' => c.info,
            _ => c.warning,
        };
        let path = change.path.to_string_lossy();
        let (folder, name) = match path.rsplit_once('/') {
            Some((folder, name)) => (Some(folder.to_owned()), name.to_owned()),
            None => (None, path.clone().into_owned()),
        };
        let deleted = status == 'D';
        let key = row.id.clone();
        let paths = vec![change.path.clone()];
        let mut actions = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .opacity(0.)
            .group_hover("change-row", |s| s.opacity(1.));
        match row.group {
            Group::Staged => {
                actions = actions.child(action_button(
                    format!("changes.unstage.{key}").into(),
                    LucideIcon::Minus,
                    "Unstage",
                    RowAction::Unstage(paths),
                ));
            }
            Group::Unstaged | Group::Untracked | Group::Conflicted => {
                if row.group != Group::Conflicted {
                    actions = actions.child(action_button(
                        format!("changes.discard.{key}").into(),
                        LucideIcon::Undo2,
                        "Discard…",
                        RowAction::Discard(Targets::from_rows([row])),
                    ));
                }
                actions = actions.child(action_button(
                    format!("changes.stage.{key}").into(),
                    LucideIcon::Plus,
                    if row.group == Group::Conflicted {
                        "Mark Resolved"
                    } else {
                        "Stage"
                    },
                    RowAction::Stage(paths),
                ));
            }
        }
        let counts = count.filter(|_| !deleted).map(|count| {
            if count.binary {
                div()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child("binary")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .gap(px(4.))
                    .text_xs()
                    .font_family(theme.fonts.mono.clone())
                    .when(count.added > 0, |this| {
                        this.child(
                            div()
                                .text_color(c.success)
                                .child(format!("+{}", count.added)),
                        )
                    })
                    .when(count.removed > 0, |this| {
                        this.child(
                            div()
                                .text_color(c.destructive)
                                .child(format!("−{}", count.removed)),
                        )
                    })
                    .into_any_element()
            }
        });
        div()
            .id(SharedString::from(format!("changes.row.{key}")))
            .group("change-row")
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                lucide(file_icon(&change.path))
                    .size(px(15.))
                    .flex_shrink_0()
                    .text_color(c.muted_foreground),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .when(deleted, |this| {
                        this.line_through().text_color(c.muted_foreground)
                    })
                    .child(name),
            )
            .when_some(change.original_path.as_ref(), |this, original| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!("← {}", original.to_string_lossy())),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .when_some(
                        folder.filter(|_| show_folder),
                        gpui_kit::ParentElement::child,
                    ),
            )
            .children(counts)
            .child(actions)
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .bg(status_color.opacity(0.15))
                    .text_color(status_color)
                    .font_family(theme.fonts.mono.clone())
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .child(status.to_string()),
            )
            .into_any_element()
    }

    fn render_tree(&self, theme: &UiTheme, cx: &mut Context<Self>) -> Tree {
        // Group headings are the tree's top folders and share their ids.
        let rows: Rc<HashMap<SharedString, ChangeRow>> = Rc::new(
            self.rows
                .iter()
                .map(|row| (row.id.clone(), row.clone()))
                .collect(),
        );
        let row_theme = theme.clone();
        let view = cx.entity().downgrade();
        let row_view = view.clone();
        let counts = self.counts.clone();
        Tree::new(
            "changes.tree",
            "Changes",
            self.tree.clone(),
            move |row: &TreeRow, _, _| {
                if let Some(change) = rows.get(&row.id) {
                    return Self::render_row(
                        change,
                        &row_theme,
                        false,
                        row_count(&counts, change),
                        &row_view,
                    );
                }
                let c = &row_theme.colors;
                match &row.kind {
                    TreeRowKind::Folder { .. } if row.depth == 0 => div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(c.muted_foreground)
                        .child(row.label.to_uppercase())
                        .into_any_element(),
                    _ => div()
                        .min_w_0()
                        .truncate()
                        .text_color(c.muted_foreground)
                        .child(row.label.clone())
                        .into_any_element(),
                }
            },
        )
        .empty(
            div()
                .p(px(16.))
                .text_sm()
                .text_color(theme.colors.muted_foreground)
                .child("No changes. The work tree matches the last commit."),
        )
        .on_event(move |event, _, cx| {
            let _ = view.update(cx, |view, cx| match event {
                TreeEvent::SelectionChanged | TreeEvent::FocusChanged => {
                    view.rebuild_menu(cx);
                    view.emit_focus(cx);
                    cx.notify();
                }
                TreeEvent::Activate(id) => {
                    if view
                        .rows
                        .iter()
                        .any(|row| row.id == *id && row.change.is_some())
                    {
                        view.toggle_selection(cx);
                    }
                }
                TreeEvent::ExpansionChanged { id, expanded } => {
                    if *expanded {
                        view.collapsed.remove(id);
                    } else {
                        view.collapsed.insert(id.clone());
                    }
                }
                TreeEvent::LoadChildren(_) => {}
            });
        })
    }

    fn render_discard(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let description = discard_description(&self.pending_discard);
        let popup = confirm::popup(
            "changes.discard",
            "Discard changes?",
            format!("{description} This can't be undone."),
            cx,
        )
        .child(
            Confirm::new(
                "changes.discard",
                "Discard",
                |this: &mut Self, window, cx| this.discard.close(window, cx),
                |this, window, cx| {
                    this.discard.close(window, cx);
                    let targets = std::mem::take(&mut this.pending_discard);
                    this.discard(targets, cx);
                },
            )
            .footer(cx),
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
        let model = self.model.clone();
        let composer = self
            .composer
            .get_or_insert_with(|| cx.new(|cx| CommitComposer::new(model, window, cx)))
            .clone();
        let filter = self.ensure_filter(window, cx);
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let total = self
            .model
            .read(cx)
            .snapshot()
            .map_or(0, |s| s.changes.len());
        let empty = if total == 0 {
            "No changes. The work tree matches the last commit."
        } else {
            "No changed paths match the filter."
        };
        let rows = self.rows.clone();
        let row_theme = theme.clone();
        let view = cx.entity().downgrade();
        let row_view = view.clone();
        let counts = self.counts.clone();
        let list = VirtualList::new(
            "changes.list",
            "Changes",
            self.list.clone(),
            move |row: &ListRow, _, _| {
                rows.get(row.index).map_or_else(
                    || div().into_any_element(),
                    |r| Self::render_row(r, &row_theme, true, row_count(&counts, r), &row_view),
                )
            },
        )
        .empty(
            div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child(empty),
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
        let list = if self.tree_mode {
            self.render_tree(&theme, cx).into_any_element()
        } else {
            list.into_any_element()
        };
        let operation = self.model.read(cx).snapshot().and_then(|s| s.operation);
        let conflicts = self
            .rows
            .iter()
            .filter(|r| r.group == Group::Conflicted && r.change.is_some())
            .count();
        let banner = operation.map(|operation| self.render_operation(operation, conflicts, cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .children(banner)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(c.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&filter).aria_label("Filter changes")),
                    )
                    .child(
                        ToggleGroup::new("changes.mode")
                            .aria_label("Show changes as")
                            .segmented(true)
                            .value([if self.tree_mode { "tree" } else { "list" }])
                            .item(
                                ToggleGroupItem::new("changes.mode.list", "list")
                                    .aria_label("List")
                                    .child(
                                        lucide(LucideIcon::List)
                                            .size(px(14.))
                                            .text_color(c.foreground),
                                    ),
                            )
                            .item(
                                ToggleGroupItem::new("changes.mode.tree", "tree")
                                    .aria_label("Tree")
                                    .child(
                                        lucide(LucideIcon::FolderTree)
                                            .size(px(14.))
                                            .text_color(c.foreground),
                                    ),
                            )
                            .on_change({
                                let view = cx.entity().downgrade();
                                move |values, _, cx| {
                                    if let Some(value) = values.first() {
                                        let tree = value == "tree";
                                        let _ = view.update(cx, |view, cx| {
                                            view.set_tree_mode(tree, cx);
                                        });
                                    }
                                }
                            }),
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
            .child(composer)
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
    fn the_tree_nests_folders_under_each_group() {
        let untracked = ChangeKind::Untracked;
        let rows = group_rows(&[
            change("src/ui/a.rs", untracked),
            change("src/b.rs", untracked),
            change("README.md", untracked),
        ]);
        let nodes = tree_nodes(&rows);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.as_ref(), "untracked:");
        assert_eq!(nodes[0].label.as_ref(), "Untracked (3)");
        let crate::ui::tree::TreeChildren::Loaded(top) = &nodes[0].children else {
            panic!("the group is a folder");
        };
        let ids: Vec<&str> = top.iter().map(|n| n.id.as_ref()).collect();
        // Folders first, then files; leaves keep the list's row ids.
        assert_eq!(ids, ["untracked:src/", "untracked:README.md"]);
        let mut folders = Vec::new();
        folder_ids(&nodes, &mut folders);
        assert_eq!(
            folders,
            ["untracked:", "untracked:src/", "untracked:src/ui/"]
        );
        assert!(covers("untracked:src/", "untracked:src/ui/a.rs"));
        assert!(covers("untracked:", "untracked:README.md"));
        assert!(!covers("untracked:src/", "untracked:srcx/a.rs"));
        assert!(!covers("untracked:src/b.rs", "untracked:src/b.rs.orig"));
    }

    #[test]
    fn ten_thousand_changes_build_quickly() {
        let changes: Vec<Change> = (0..10_000)
            .map(|i| {
                change(
                    &format!("dir{}/sub{}/file{i}.txt", i % 50, i % 7),
                    ChangeKind::Untracked,
                )
            })
            .collect();
        let started = std::time::Instant::now();
        let rows = group_rows(&changes);
        let nodes = tree_nodes(&rows);
        let state = TreeState::new(nodes, []).unwrap();
        state.set_expanded(&"untracked:".into(), true);
        assert_eq!(rows.len(), 10_001);
        // Generous for a debug build on a shared CI runner.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
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
        fn counts_lines_filters_and_focuses_the_first_change(cx: &mut TestAppContext) {
            cx.update(crate::ui::theme::init);
            let dir = tempfile::tempdir().unwrap();
            let git = Git::locate(None).unwrap();
            let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
            let run = |args: &[&str]| {
                let mut full = vec![
                    "-c",
                    "user.name=Ada",
                    "-c",
                    "user.email=ada@example.com",
                    "-c",
                    "commit.gpgsign=false",
                ];
                full.extend_from_slice(args);
                git.run(&git.command(full).cwd(dir.path())).unwrap();
            };
            std::fs::create_dir(dir.path().join("src")).unwrap();
            std::fs::write(dir.path().join("src/a.rs"), "one\ntwo\n").unwrap();
            run(&["add", "."]);
            run(&["commit", "-qm", "base"]);
            std::fs::write(dir.path().join("src/a.rs"), "one\nthree\nfour\n").unwrap();
            std::fs::write(dir.path().join("notes.md"), "n").unwrap();
            let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
            let view = cx.new(|cx| ChangesView::new(model.clone(), cx));
            cx.run_until_parked();

            view.read_with(cx, |view, _| {
                let row = view
                    .rows()
                    .iter()
                    .find(|r| r.id.as_ref() == "unstaged:src/a.rs")
                    .unwrap();
                let count = view.line_count(row).unwrap();
                assert_eq!((count.added, count.removed), (2, 1));
                // The first change is shown without a click.
                assert_eq!(
                    view.focused_row().map(|r| r.id.to_string()).as_deref(),
                    Some("unstaged:src/a.rs")
                );
            });

            view.update(cx, |view, cx| view.set_filter("NOTES", cx));
            view.read_with(cx, |view, _| {
                let ids: Vec<&str> = view.rows().iter().map(|r| r.id.as_ref()).collect();
                assert_eq!(ids, ["untracked:", "untracked:notes.md"]);
            });
        }

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

        #[gpui_kit::test]
        fn a_folder_in_the_tree_stages_everything_under_it(cx: &mut TestAppContext) {
            cx.update(crate::ui::theme::init);
            let dir = tempfile::tempdir().unwrap();
            let git = Git::locate(None).unwrap();
            let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
            std::fs::create_dir_all(dir.path().join("src/ui")).unwrap();
            std::fs::write(dir.path().join("src/ui/a.rs"), "a").unwrap();
            std::fs::write(dir.path().join("src/b.rs"), "b").unwrap();
            std::fs::write(dir.path().join("README.md"), "r").unwrap();
            let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
            let view = cx.new(|cx| ChangesView::new(model.clone(), cx));
            cx.run_until_parked();

            view.update(cx, |view, cx| {
                view.set_tree_mode(true, cx);
                assert!(view.tree_mode());
                assert!(view.tree().is_expanded(&"untracked:".into()));
                let untracked: Vec<String> = view
                    .rows()
                    .iter()
                    .filter(|r| r.change.is_some())
                    .map(|r| r.id.to_string())
                    .collect();
                assert!(untracked.iter().any(|id| id.starts_with("untracked:src/")));
                view.tree().set_selected(["untracked:src/".into()]);
                assert!(
                    view.selection()
                        .untracked
                        .iter()
                        .all(|p| p.starts_with("src"))
                );
                view.toggle_selection(cx);
            });
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                let staged: Vec<String> = view
                    .rows()
                    .iter()
                    .filter(|r| r.group == Group::Staged && r.change.is_some())
                    .map(|r| r.id.to_string())
                    .collect();
                assert_eq!(staged, ["staged:src/b.rs", "staged:src/ui/a.rs"]);
                assert!(view.tree().is_expanded(&"staged:src/ui/".into()));
            });

            // Switching back keeps the selection.
            view.update(cx, |view, cx| {
                view.tree().set_selected(["untracked:README.md".into()]);
                view.set_tree_mode(false, cx);
                assert_eq!(view.selection().untracked, [PathBuf::from("README.md")]);
            });
        }

        #[gpui_kit::test]
        fn conflicts_resolve_with_a_side_and_the_merge_continues(cx: &mut TestAppContext) {
            cx.update(crate::ui::theme::init);
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let git = Git::locate(None).unwrap();
            let repository = Repository::init(&git, root, Some("main")).unwrap();
            let run = |args: &[&str]| {
                git.run(&git.command(args.iter().copied()).cwd(root))
                    .unwrap();
            };
            for (key, value) in [
                ("user.name", "Bonsai"),
                ("user.email", "bonsai@example.com"),
                ("commit.gpgsign", "false"),
            ] {
                run(&["config", key, value]);
            }
            std::fs::write(root.join("a.txt"), "base").unwrap();
            run(&["add", "."]);
            run(&["commit", "-qm", "base"]);
            run(&["switch", "-qc", "topic"]);
            std::fs::write(root.join("a.txt"), "topic").unwrap();
            run(&["commit", "-qam", "topic"]);
            run(&["switch", "-q", "main"]);
            std::fs::write(root.join("a.txt"), "main").unwrap();
            run(&["commit", "-qam", "main"]);
            assert_eq!(
                repository
                    .merge("topic", bonsai_git::MergeMode::Default)
                    .unwrap(),
                bonsai_git::Integration::Conflicts
            );

            let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
            let view = cx.new(|cx| ChangesView::new(model.clone(), cx));
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert!(view.rows().iter().any(|r| r.group == Group::Conflicted));
            });
            view.update(cx, |view, cx| {
                view.resolve_with(vec![PathBuf::from("a.txt")], Side::Ours, cx);
            });
            cx.run_until_parked();
            assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "main");
            view.update(cx, |view, cx| {
                view.resolve_operation(OperationAction::Continue, cx);
            });
            cx.run_until_parked();
            model.read_with(cx, |model, _| {
                assert_eq!(model.snapshot().unwrap().operation, None);
            });
        }
    }
}
