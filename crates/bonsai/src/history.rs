//! The History view: every branch's commits as a graph with ref labels,
//! paged in as you scroll, with search and commit actions (CONTEXT.md:
//! History).

use std::{collections::HashMap, path::PathBuf, rc::Rc, time::Duration};

use bonsai_git::{
    BisectVerdict, CommitInfo, Head, LogFilter, LogReader, LogScope, Oid, RebaseAction, ResetMode,
    Snapshot,
};
use bonsai_graph::{Commit, Layout, Row};
use gpui_icons::LucideIcon;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, ElementId, Entity, EventEmitter, IntoElement,
    ParentElement as _, Render, SharedString, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    activity_panel::ago,
    avatar::avatar,
    refs::{Checkout, RefDialog, RefsPanel},
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        commit_graph::{
            CommitGraph, CommitGraphState, GraphEdge, GraphLabel, GraphLabelKind, GraphNodeKind,
            GraphRow,
        },
        context_menu::context_menu,
        input::{Input, InputEvent, InputState},
        menu::{Menu, MenuItem, MenuState},
        theme::UiTheme,
        virtual_list::{ListRow, ListSelectionMode, VirtualListEvent},
    },
    workspace::title_bar::ghost_trigger,
};

/// Commits loaded per page.
pub const PAGE: usize = 500;

/// Ref labels drawn on a row before the rest collapse into "+N".
const MAX_LABELS: usize = 2;

/// How far a jump to a commit pages in before giving up. Pages are cheap
/// to read (one `git log` streams them), so this only stops runaway paging.
const MAX_SEARCH_DEPTH: usize = 1_000_000;

/// Rows from the end at which the next page starts loading.
const PREFETCH: usize = 100;

pub enum HistoryEvent {
    /// Show this commit's diff, or nothing.
    Selected(Option<(Oid, SharedString)>),
    /// Open Edit Commits.
    EditCommits(EditCommits),
    /// Show this commit's files in Files.
    BrowseFiles(Oid),
    /// Mark this commit good or bad in a bisect, starting one if needed.
    Bisect(Oid, BisectVerdict),
    /// Open Compare with the newer commit (second) against the older.
    Compare(Oid, Oid),
}

/// An interactive rebase to plan: the commits after `base` (`None`: every
/// commit), with `preset` applied to some of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditCommits {
    pub base: Option<Oid>,
    pub preset: Vec<(Oid, RebaseAction)>,
}

/// Which commits History shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HistoryScope {
    /// Every branch, remote branch and tag.
    #[default]
    All,
    /// The current branch, and its upstream so incoming commits show too.
    Current,
    /// One local branch, and its upstream.
    Branch(String),
}

impl HistoryScope {
    /// The refs `git log` reads for this scope.
    #[must_use]
    pub fn log_scope(&self, snapshot: Option<&Snapshot>) -> LogScope {
        let with_upstream = |name: &str, start: &str| {
            let mut refs = vec![start.to_owned()];
            if let Some(upstream) = snapshot
                .and_then(|s| s.branches.iter().find(|b| b.name == name))
                .and_then(|b| b.upstream.as_ref())
                .filter(|u| !u.gone)
            {
                refs.push(format!("refs/remotes/{}", upstream.name));
            }
            LogScope::Refs(refs)
        };
        match self {
            Self::All => LogScope::All,
            Self::Current => match snapshot.map(|s| &s.head) {
                Some(Head::Branch { name, oid: Some(_) }) => with_upstream(name, "HEAD"),
                _ => LogScope::Head,
            },
            Self::Branch(name) => with_upstream(name, &format!("refs/heads/{name}")),
        }
    }

    /// What the picker shows.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::All => "All Branches".to_owned(),
            Self::Current => "Current Branch".to_owned(),
            Self::Branch(name) => name.clone(),
        }
    }
}

/// What the search field asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    pub filter: LogFilter,
    /// A commit hash (or its start) to jump to.
    pub hash: Option<String>,
}

/// Reads the search field: `author:` and `path:` narrow by author or path,
/// `code:` finds commits that add or remove that text,
/// a hexadecimal word of 7 or more characters jumps to that commit, and any
/// other text searches commit messages.
#[must_use]
pub fn parse_search(text: &str) -> Search {
    let text = text.trim();
    let mut search = Search::default();
    if text.is_empty() {
        return search;
    }
    if let Some(author) = text.strip_prefix("author:") {
        search.filter.author = Some(author.trim().to_owned()).filter(|a| !a.is_empty());
    } else if let Some(code) = text.strip_prefix("code:") {
        search.filter.code = Some(code.trim().to_owned()).filter(|c| !c.is_empty());
    } else if let Some(path) = text.strip_prefix("path:") {
        search.filter.path = Some(PathBuf::from(path.trim())).filter(|p| !p.as_os_str().is_empty());
    } else if text.len() >= 7 && text.len() <= 40 && text.chars().all(|c| c.is_ascii_hexdigit()) {
        search.hash = Some(text.to_ascii_lowercase());
    } else {
        search.filter.message = Some(text.to_owned());
    }
    search
}

/// Ref labels by the commit they point at.
#[must_use]
pub fn labels(snapshot: &Snapshot) -> HashMap<Oid, Vec<GraphLabel>> {
    let mut labels: HashMap<Oid, Vec<GraphLabel>> = HashMap::new();
    let mut add = |oid: &Oid, kind, text: String| {
        labels
            .entry(oid.clone())
            .or_default()
            .push(GraphLabel::new(kind, text));
    };
    match &snapshot.head {
        Head::Branch {
            name,
            oid: Some(oid),
        } => add(oid, GraphLabelKind::Head, name.clone()),
        Head::Detached { oid } => add(oid, GraphLabelKind::Head, "HEAD".to_owned()),
        Head::Branch { oid: None, .. } => {}
    }
    for branch in snapshot.branches.iter().filter(|b| !b.is_head) {
        add(&branch.oid, GraphLabelKind::Branch, branch.name.clone());
    }
    // A remote branch level with the local branch of the same name adds
    // nothing but width: the local label stands for both.
    let in_sync = |remote: &bonsai_git::RemoteBranch| {
        snapshot
            .branches
            .iter()
            .any(|b| b.name == remote.name && b.oid == remote.oid)
    };
    for remote in snapshot
        .remote_branches
        .iter()
        .filter(|r| r.name != "HEAD" && !in_sync(r))
    {
        add(
            &remote.oid,
            GraphLabelKind::Remote,
            format!("{}/{}", remote.remote, remote.name),
        );
    }
    for tag in &snapshot.tags {
        add(&tag.target, GraphLabelKind::Tag, tag.name.clone());
    }
    labels
}

fn graph_row(commit: &CommitInfo, row: &Row, labels: &HashMap<Oid, Vec<GraphLabel>>) -> GraphRow {
    let mut graph = GraphRow::new(
        ElementId::Name(commit.oid.as_str().to_owned().into()),
        commit.summary.clone(),
        row.lane,
    )
    .kind(if row.merge {
        GraphNodeKind::Merge
    } else {
        GraphNodeKind::Commit
    })
    .color(row.color);
    for edge in &row.incoming {
        graph = graph.incoming(GraphEdge::new(edge.from, edge.to, edge.color));
    }
    for edge in &row.outgoing {
        graph = graph.outgoing(GraphEdge::new(edge.from, edge.to, edge.color));
    }
    for label in labels
        .get(&commit.oid)
        .into_iter()
        .flatten()
        .take(MAX_LABELS)
    {
        graph = graph.label(label.clone());
    }
    graph
}

/// A menu item's work on the refs panel or on this view.
type RefsAction = Box<dyn Fn(&mut RefsPanel, &mut Context<RefsPanel>)>;
type ViewAction = Box<dyn Fn(&mut HistoryView, &mut Context<HistoryView>)>;

/// The operation name for [`HistoryView::fetch_full_history`], which
/// History and the diff pane watch for to reload.
pub(crate) const FETCH_FULL_HISTORY: &str = "Fetch full history";

/// Where paging through the history stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Paging {
    /// Another page may follow.
    More,
    /// A page is loading.
    Loading,
    /// The history ended.
    End,
}

pub struct HistoryView {
    model: Entity<RepositoryModel>,
    refs: Entity<RefsPanel>,
    graph: CommitGraphState,
    commits: Rc<Vec<CommitInfo>>,
    layout: Layout<Oid>,
    /// The `git log` the next page comes from; taken while a page loads.
    reader: Option<LogReader>,
    /// A commit to page to after a reload, so the selection survives.
    keep: Option<String>,
    /// How far a reload pages looking for `keep` before giving up on it.
    keep_depth: usize,
    labels: HashMap<Oid, Vec<GraphLabel>>,
    search: Search,
    paging: Paging,
    /// Whether the repository is a shallow clone, so the history stops
    /// short of its first commit.
    shallow: bool,
    error: Option<SharedString>,
    /// Ignores pages for a search or refresh that has since changed.
    generation: u64,
    /// Whether the view has loaded at all; it waits until first shown.
    started: bool,
    /// The refs the loaded history reflects, to know when to reload.
    refs_key: Option<String>,
    /// Built on first render, which has the window an input needs.
    search_input: Option<Entity<InputState>>,
    /// Text to put in the search field on the next render, which has the
    /// window.
    search_field_text: Option<String>,
    menu: Entity<MenuState>,
    scope: HistoryScope,
    scope_menu: Entity<MenuState>,
    tasks: Vec<Task<()>>,
    subscriptions: Vec<Subscription>,
}

/// `git format-patch`'s file name for a commit, such as
/// `0001-Fix-the-parser.patch`, with its short hash instead of the number.
#[must_use]
pub fn patch_file_name(oid: &Oid, summary: &str) -> String {
    let mut slug = String::new();
    for c in summary.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 52 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("{}.patch", oid.short())
    } else {
        format!("{}-{slug}.patch", oid.short())
    }
}

/// A menu click that updates the history view.
fn with_this(
    this: &gpui_kit::WeakEntity<HistoryView>,
    f: impl Fn(&mut HistoryView, &mut Context<HistoryView>) + 'static,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
    let this = this.clone();
    move |_, _, cx| {
        let _ = this.update(cx, |view, cx| f(view, cx));
    }
}
impl EventEmitter<HistoryEvent> for HistoryView {}

/// What changing refs looks like, so a reload happens only when they move.
fn refs_key(snapshot: &Snapshot) -> String {
    let mut key = format!("{:?}", snapshot.head);
    for b in &snapshot.branches {
        key.push_str(&b.name);
        key.push_str(b.oid.as_str());
    }
    for r in &snapshot.remote_branches {
        key.push_str(&r.remote);
        key.push_str(&r.name);
        key.push_str(r.oid.as_str());
    }
    for t in &snapshot.tags {
        key.push_str(&t.name);
        key.push_str(t.target.as_str());
    }
    key
}

/// Edit Commits squashing `commits` (newest first) into the oldest, when
/// they are a run of commits each the parent of the one before, with no
/// merges.
fn squash_request(commits: &[&CommitInfo]) -> Option<EditCommits> {
    let (oldest, newer) = commits.split_last()?;
    let linear = commits.iter().all(|c| c.parents.len() == 1)
        && commits
            .windows(2)
            .all(|pair| pair[0].parents.first() == Some(&pair[1].oid));
    (linear && !newer.is_empty()).then(|| EditCommits {
        base: oldest.parents.first().cloned(),
        preset: newer
            .iter()
            .map(|c| (c.oid.clone(), RebaseAction::Squash))
            .collect(),
    })
}

impl HistoryView {
    pub fn new(
        model: Entity<RepositoryModel>,
        refs: Entity<RefsPanel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let graph = CommitGraphState::new(Vec::new()).expect("an empty graph has no duplicates");
        graph.set_selection_mode(ListSelectionMode::Multiple);
        let menu = cx.new(|cx| MenuState::new(Vec::new(), cx));
        let scope_menu = cx.new(|cx| MenuState::new(Vec::new(), cx).width(px(240.)));
        let subscriptions = vec![
            cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
                if !this.started {
                    return;
                }
                match event {
                    RepositoryEvent::SnapshotChanged => this.refresh_if_refs_moved(cx),
                    // Unshallowing adds commits without moving a ref.
                    RepositoryEvent::OperationFinished {
                        name, error: None, ..
                    } if name.as_ref() == FETCH_FULL_HISTORY => this.reload(cx),
                    RepositoryEvent::OperationFinished { .. } => {}
                }
            }),
        ];
        Self {
            model,
            refs,
            graph,
            commits: Rc::new(Vec::new()),
            layout: Layout::new(),
            reader: None,
            keep: None,
            keep_depth: 0,
            labels: HashMap::new(),
            search: Search::default(),
            paging: Paging::More,
            shallow: false,
            error: None,
            generation: 0,
            started: false,
            refs_key: None,
            search_input: None,
            search_field_text: None,
            menu,
            scope: HistoryScope::All,
            scope_menu,
            tasks: Vec::new(),
            subscriptions,
        }
    }

    #[must_use]
    pub fn commits(&self) -> &[CommitInfo] {
        &self.commits
    }

    #[must_use]
    pub fn graph(&self) -> &CommitGraphState {
        &self.graph
    }

    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.paging == Paging::Loading
    }

    /// Whether the history stops short because this is a shallow clone.
    #[must_use]
    pub fn is_shallow(&self) -> bool {
        self.shallow
    }

    /// Fetches the commits a shallow clone left out; History reloads once
    /// they arrive.
    pub fn fetch_full_history(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| {
            model.run_network(
                FETCH_FULL_HISTORY,
                |repository, _, _| repository.fetch_full_history(),
                cx,
            );
        });
    }

    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.paging == Paging::End
    }

    /// Loads the first page the first time the view is shown.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if !self.started {
            self.started = true;
            self.reload(cx);
        }
    }

    fn refresh_if_refs_moved(&mut self, cx: &mut Context<Self>) {
        let key = self.model.read(cx).snapshot().map(refs_key);
        if key != self.refs_key {
            self.reload(cx);
        }
    }

    /// Reads the history again from the top, keeping as many commits as are
    /// loaded now (so the scroll position and selection survive).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        // Read the first page again, then page on to the selected commit:
        // each page is small, so the window never waits on a whole history.
        self.keep = self.selected().map(|c| c.oid.as_str().to_owned());
        // A commit a rewrite replaced is gone for good; don't page through
        // the whole history looking for it.
        self.keep_depth = self.commits.len() + PAGE;
        self.paging = Paging::More;
        self.load(0, PAGE, cx);
    }

    /// Loads the next page, unless one is loading or the history ended.
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.paging != Paging::More || !self.started {
            return;
        }
        self.load(self.commits.len(), PAGE, cx);
    }

    fn load(&mut self, skip: usize, limit: usize, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.paging = Paging::Loading;
        if skip == 0 {
            self.rebuild_scope_menu(cx);
        }
        let snapshot = self.model.read(cx).snapshot();
        self.refs_key = snapshot.map(refs_key);
        if skip == 0 {
            self.labels = snapshot.map(labels).unwrap_or_default();
        }
        let repository = self.model.read(cx).repository().clone();
        let filter = self.search.filter.clone();
        let scope = self.scope.log_scope(snapshot);
        // A new read for the first page; otherwise the next page of the
        // current one.
        let reader = if skip == 0 { None } else { self.reader.take() };
        if skip > 0 && reader.is_none() {
            self.paging = Paging::End;
            cx.notify();
            return;
        }
        let work = cx.background_spawn(async move {
            let mut reader = match reader {
                Some(reader) => reader,
                None => repository.log_reader(&scope, &filter)?,
            };
            let page = reader.next_page(limit)?;
            let shallow = (skip == 0).then(|| repository.is_shallow().unwrap_or(false));
            Ok::<_, bonsai_git::Error>((reader, page, shallow))
        });
        self.tasks.retain(|task| !task.is_ready());
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.paging = Paging::More;
                match result {
                    Ok((reader, page, shallow)) => {
                        this.error = None;
                        if let Some(shallow) = shallow {
                            this.shallow = shallow;
                        }
                        if page.len() < limit || reader.is_done() {
                            this.paging = Paging::End;
                        } else {
                            this.reader = Some(reader);
                        }
                        this.apply(skip, page, cx);
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn apply(&mut self, skip: usize, page: Vec<CommitInfo>, cx: &mut Context<Self>) {
        if skip == 0 {
            self.layout = Layout::new();
            self.commits = Rc::new(Vec::new());
        }
        let commits: Vec<Commit<Oid>> = page
            .iter()
            .map(|c| Commit {
                id: c.oid.clone(),
                parents: c.parents.clone(),
            })
            .collect();
        let laid = self.layout.extend(&commits);
        let rows: Vec<GraphRow> = page
            .iter()
            .zip(&laid)
            .map(|(commit, row)| graph_row(commit, row, &self.labels))
            .collect();
        let first = rows.first().map(|row| row.id.clone());
        // IDs are commit hashes, which a log lists once.
        let _ = if skip == 0 {
            self.graph.replace_rows(rows)
        } else {
            self.graph.append_rows(rows)
        };
        Rc::make_mut(&mut self.commits).extend(page);
        // Start on the newest commit, so its diff shows at once.
        if skip == 0
            && self.keep.is_none()
            && self.graph.selected().is_empty()
            && let Some(first) = first
        {
            self.graph.set_selected([first]);
        }
        let target = self.search.hash.clone().or_else(|| self.keep.clone());
        if let Some(hash) = target {
            if self.reveal_hash(&hash, cx) {
                self.keep = None;
            } else if self.paging != Paging::End
                && self.commits.len()
                    < if self.search.hash.is_some() {
                        MAX_SEARCH_DEPTH
                    } else {
                        self.keep_depth
                    }
            {
                // Not loaded yet: keep paging until it turns up.
                self.load(self.commits.len(), PAGE, cx);
                return;
            } else if self.keep.take().is_some()
                && let Some(first) = self.graph.row(0)
            {
                // The kept commit is gone: start on the newest again.
                self.graph.set_selected([first.id.clone()]);
                self.graph.reveal(&first.id);
            }
        }
        self.rebuild_menu(cx);
        self.emit_selection(cx);
    }

    /// Selects and scrolls to the loaded commit whose hash starts with
    /// `hash`. Returns whether one was found.
    pub fn reveal_hash(&mut self, hash: &str, cx: &mut Context<Self>) -> bool {
        let Some(commit) = self
            .commits
            .iter()
            .find(|c| c.oid.as_str().starts_with(hash))
        else {
            return false;
        };
        let id = ElementId::Name(commit.oid.as_str().to_owned().into());
        self.graph.set_selected([id.clone()]);
        self.graph.reveal(&id);
        self.rebuild_menu(cx);
        self.emit_selection(cx);
        true
    }

    /// Selects `oid`, paging in history until it is loaded. Clears a search
    /// that would hide it.
    pub fn reveal(&mut self, oid: &Oid, cx: &mut Context<Self>) {
        self.started = true;
        self.search_field_text = Some(String::new());
        // A commit outside the chosen branch would never page in.
        let had_filter = !self.search.filter.is_empty() || self.scope != HistoryScope::All;
        self.scope = HistoryScope::All;
        self.search = Search {
            filter: LogFilter::default(),
            hash: Some(oid.as_str().to_owned()),
        };
        if had_filter || self.commits.is_empty() {
            self.paging = Paging::More;
            self.load(0, PAGE, cx);
        } else if !self.reveal_hash(oid.as_str(), cx) && self.paging != Paging::End {
            self.load(self.commits.len(), PAGE, cx);
        }
    }

    /// Shows the commits of `scope` only.
    pub fn set_scope(&mut self, scope: HistoryScope, cx: &mut Context<Self>) {
        if scope == self.scope {
            return;
        }
        self.scope = scope;
        self.started = true;
        self.paging = Paging::More;
        self.load(0, PAGE, cx);
        cx.notify();
    }

    #[must_use]
    pub fn scope(&self) -> &HistoryScope {
        &self.scope
    }

    /// The scope picker: every branch, the current one, or a local branch.
    fn rebuild_scope_menu(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let pick = |scope: HistoryScope| {
            let this = this.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                let scope = scope.clone();
                let _ = this.update(cx, |view, cx| view.set_scope(scope, cx));
            }
        };
        let current = self.scope.clone();
        let mut items = vec![
            MenuItem::new("history.scope.all", "All Branches")
                .icon(LucideIcon::GitFork)
                .checked(current == HistoryScope::All)
                .on_click(pick(HistoryScope::All)),
            MenuItem::new("history.scope.current", "Current Branch")
                .icon(LucideIcon::GitBranch)
                .checked(current == HistoryScope::Current)
                .on_click(pick(HistoryScope::Current)),
        ];
        let branches: Vec<String> = self
            .model
            .read(cx)
            .snapshot()
            .map(|s| s.branches.iter().map(|b| b.name.clone()).collect())
            .unwrap_or_default();
        if !branches.is_empty() {
            items.push(MenuItem::separator());
            items.push(MenuItem::label("history.scope.branches", "Branches"));
        }
        for name in branches {
            let scope = HistoryScope::Branch(name.clone());
            items.push(
                MenuItem::new(
                    SharedString::from(format!("history.scope.branch.{name}")),
                    name,
                )
                .icon(LucideIcon::GitBranch)
                .checked(current == scope)
                .on_click(pick(scope)),
            );
        }
        self.scope_menu
            .update(cx, |menu, cx| menu.set_items(items, cx));
    }

    /// Searches for `text` and shows it in the search field.
    pub fn search_for(&mut self, text: &str, cx: &mut Context<Self>) {
        self.started = true;
        self.search_field_text = Some(text.to_owned());
        self.set_search(text, cx);
    }

    /// Applies the search field's text.
    pub fn set_search(&mut self, text: &str, cx: &mut Context<Self>) {
        let search = parse_search(text);
        if search == self.search {
            return;
        }
        let refilter = search.filter != self.search.filter;
        self.search = search;
        if refilter {
            self.paging = Paging::More;
            self.load(0, PAGE, cx);
        } else if let Some(hash) = self.search.hash.clone() {
            self.reveal_hash(&hash, cx);
        }
    }

    /// The selected commit: the focused one when several are selected, so
    /// the diff pane follows the keyboard.
    #[must_use]
    pub fn selected(&self) -> Option<&CommitInfo> {
        let selected = self.graph.selected();
        let id = self
            .graph
            .focused()
            .filter(|id| selected.contains(id))
            .or_else(|| selected.into_iter().next())?;
        let ElementId::Name(name) = id else {
            return None;
        };
        self.commits
            .iter()
            .find(|c| c.oid.as_str() == name.as_ref())
    }

    /// Every selected commit, newest first as History lists them.
    #[must_use]
    pub fn selected_commits(&self) -> Vec<&CommitInfo> {
        let selected = self.graph.selected();
        self.commits
            .iter()
            .filter(|c| {
                selected
                    .iter()
                    .any(|id| matches!(id, ElementId::Name(n) if n.as_ref() == c.oid.as_str()))
            })
            .collect()
    }

    /// Tells the workspace what to show in the diff pane.
    pub fn emit_selection(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .selected()
            .map(|c| (c.oid.clone(), SharedString::from(c.summary.clone())));
        cx.emit(HistoryEvent::Selected(selected));
    }

    /// Selects and scrolls to the commit `HEAD` points at.
    pub fn reveal_head(&mut self, cx: &mut Context<Self>) {
        let oid = match &self.model.read(cx).snapshot().map(|s| &s.head) {
            Some(Head::Branch { oid: Some(oid), .. } | Head::Detached { oid }) => oid.clone(),
            _ => return,
        };
        self.reveal(&oid, cx);
    }

    fn current_branch(&self, cx: &gpui_kit::App) -> Option<String> {
        match &self.model.read(cx).snapshot()?.head {
            Head::Branch { name, .. } => Some(name.clone()),
            Head::Detached { .. } => None,
        }
    }

    /// Cherry-picks the commit onto the current branch.
    pub fn cherry_pick(&mut self, oid: Oid, cx: &mut Context<Self>) {
        let name = format!("Cherry-pick {}", short(&oid));
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.cherry_pick(&[oid.as_str()]).map(drop),
                cx,
            );
        });
    }

    /// Cherry-picks `oids` onto the current branch, oldest first.
    pub fn cherry_pick_all(&mut self, oids: Vec<Oid>, cx: &mut Context<Self>) {
        let name = format!("Cherry-pick {} commits", oids.len());
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| {
                    let hashes: Vec<&str> = oids.iter().rev().map(Oid::as_str).collect();
                    repository.cherry_pick(&hashes).map(drop)
                },
                cx,
            );
        });
    }

    /// Reverts `oids`, newest first.
    pub fn revert_all(&mut self, oids: Vec<Oid>, cx: &mut Context<Self>) {
        let name = format!("Revert {} commits", oids.len());
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| {
                    let hashes: Vec<&str> = oids.iter().map(Oid::as_str).collect();
                    repository.revert_commits(&hashes).map(drop)
                },
                cx,
            );
        });
    }

    /// The menu for several selected commits.
    fn multi_menu(&self, commits: &[&CommitInfo], cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let oids: Vec<Oid> = commits.iter().map(|c| c.oid.clone()).collect();
        let count = oids.len();
        let has_merge = commits.iter().any(|c| c.parents.len() > 1);
        let hashes = oids.iter().map(Oid::as_str).collect::<Vec<_>>().join("\n");
        let squash = squash_request(commits);
        let mut items = vec![
            MenuItem::label("history.multi", format!("{count} commits selected")),
            MenuItem::new("history.copy-all", "Copy Hashes")
                .icon(LucideIcon::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(hashes.clone()));
                }),
        ];
        // History lists newest first, so the second is the older one.
        if let [newer, older] = &oids[..] {
            let (base, head) = (older.clone(), newer.clone());
            items.push(
                MenuItem::new("history.compare", "Compare These Commits")
                    .icon(LucideIcon::GitCompareArrows)
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            let (base, head) = (base.clone(), head.clone());
                            let _ = this.update(cx, |_, cx| {
                                cx.emit(HistoryEvent::Compare(base, head));
                            });
                        }
                    }),
            );
        }
        if let Some(current) = self.current_branch(cx) {
            items.extend([
                MenuItem::separator(),
                MenuItem::new(
                    "history.cherry-pick-all",
                    format!("Cherry-pick {count} Commits onto {current}"),
                )
                .icon(LucideIcon::Cherry)
                .disabled(has_merge)
                .on_click(with_this(&this, {
                    let oids = oids.clone();
                    move |view, cx| view.cherry_pick_all(oids.clone(), cx)
                })),
                MenuItem::new("history.squash-all", format!("Squash {count} Commits…"))
                    .icon(LucideIcon::Combine)
                    .disabled(squash.is_none())
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            if let Some(request) = squash.clone() {
                                let _ = this.update(cx, |_, cx| {
                                    cx.emit(HistoryEvent::EditCommits(request));
                                });
                            }
                        }
                    }),
                MenuItem::new("history.revert-all", format!("Revert {count} Commits"))
                    .icon(LucideIcon::Undo2)
                    .disabled(has_merge)
                    .on_click(with_this(&this, {
                        let oids = oids.clone();
                        move |view, cx| view.revert_all(oids.clone(), cx)
                    })),
            ]);
        }
        items
    }

    /// Copies the commit as an email-style patch.
    pub fn copy_patch(&mut self, oid: Oid, cx: &mut Context<Self>) {
        let repository = self.model.read(cx).repository().clone();
        let patch = cx.background_spawn(async move { repository.format_patch(oid.as_str()) });
        cx.spawn(async move |_, cx| {
            if let Ok(patch) = patch.await {
                cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(patch)));
            }
        })
        .detach();
    }

    /// Asks where to save the commit as a patch file, then writes it.
    pub fn save_patch(&mut self, oid: Oid, summary: &str, cx: &mut Context<Self>) {
        let root = self.model.read(cx).repository().root().to_path_buf();
        let path = cx.prompt_for_new_path(&root, Some(&patch_file_name(&oid, summary)));
        let model = self.model.downgrade();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            let _ = model.update(cx, |model, cx| {
                model.run(
                    format!("Save {} as a patch", short(&oid)),
                    move |repository| repository.save_patch(oid.as_str(), &path),
                    cx,
                );
            });
        })
        .detach();
    }

    /// Adds a commit that undoes the commit.
    pub fn revert(&mut self, oid: Oid, cx: &mut Context<Self>) {
        let name = format!("Revert {}", short(&oid));
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.revert(oid.as_str()).map(drop),
                cx,
            );
        });
    }

    /// Folds the staged changes into the commit, keeping its message.
    pub fn amend(&mut self, oid: Oid, cx: &mut Context<Self>) {
        let name = format!("Amend {}", short(&oid));
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.amend_commit(oid.as_str()).map(drop),
                cx,
            );
        });
    }

    /// Commits the staged changes as a `fixup!` of the commit, to be
    /// folded in later by Edit Commits.
    pub fn fixup(&mut self, oid: Oid, cx: &mut Context<Self>) {
        let name = format!("Commit fixup for {}", short(&oid));
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.fixup_commit(oid.as_str()).map(drop),
                cx,
            );
        });
    }

    /// Edit Commits and its shortcuts, for a commit that isn't a merge.
    fn edit_items(&self, commit: &CommitInfo, cx: &mut Context<Self>) -> Vec<MenuItem> {
        if commit.parents.len() > 1 {
            return Vec::new();
        }
        let this = cx.entity().downgrade();
        let base = commit.parents.first().cloned();
        let edit = |request: EditCommits| {
            let this = this.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                let _ = this.update(cx, |_, cx| {
                    cx.emit(HistoryEvent::EditCommits(request.clone()));
                });
            }
        };
        let preset = |action| EditCommits {
            base: base.clone(),
            preset: vec![(commit.oid.clone(), action)],
        };
        // Squashing into the parent edits from the grandparent, if the
        // parent is loaded and isn't a merge.
        let grandparent = base.as_ref().and_then(|parent| {
            self.commits
                .iter()
                .find(|c| c.oid == *parent)
                .filter(|c| c.parents.len() < 2)
                .map(|c| c.parents.first().cloned())
        });
        let mut items = vec![
            MenuItem::separator(),
            MenuItem::new("history.edit", "Edit Commits from Here…")
                .icon(LucideIcon::ListChecks)
                .on_click(edit(EditCommits {
                    base: base.clone(),
                    preset: Vec::new(),
                })),
            MenuItem::new("history.reword", "Reword…")
                .icon(LucideIcon::PencilLine)
                .on_click(edit(preset(RebaseAction::Reword))),
            MenuItem::new("history.amend", "Amend with Staged Changes")
                .icon(LucideIcon::GitCommitVertical)
                .on_click(with_this(&this, {
                    let oid = commit.oid.clone();
                    move |view, cx| view.amend(oid.clone(), cx)
                })),
            MenuItem::new("history.fixup", "Commit Staged as Fixup")
                .icon(LucideIcon::Bandage)
                .on_click(with_this(&this, {
                    let oid = commit.oid.clone();
                    move |view, cx| view.fixup(oid.clone(), cx)
                })),
        ];
        if let Some(grandparent) = grandparent {
            items.push(
                MenuItem::new("history.squash", "Squash into Parent…")
                    .icon(LucideIcon::Combine)
                    .on_click(edit(EditCommits {
                        base: grandparent,
                        preset: vec![(commit.oid.clone(), RebaseAction::Squash)],
                    })),
            );
        }
        items.push(
            MenuItem::new("history.drop", "Drop…")
                .icon(LucideIcon::Trash2)
                .on_click(edit(preset(RebaseAction::Drop))),
        );
        items
    }

    fn rebuild_menu(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_commits();
        if selected.len() > 1 {
            let items = self.multi_menu(&selected, cx);
            self.menu.update(cx, |menu, cx| menu.set_items(items, cx));
            return;
        }
        let Some(commit) = self.selected().cloned() else {
            self.menu
                .update(cx, |menu, cx| menu.set_items(Vec::new(), cx));
            return;
        };
        let this = cx.entity().downgrade();
        let refs = self.refs.downgrade();
        let oid = commit.oid.clone();
        let hash = oid.as_str().to_owned();
        let refs_do = |f: RefsAction| {
            let refs = refs.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                let _ = refs.update(cx, |refs, cx| f(refs, cx));
            }
        };
        let browse = oid.clone();
        let web = self
            .model
            .read(cx)
            .snapshot()
            .and_then(|s| crate::web::repository_web(s, cx));
        let mut items = vec![
            MenuItem::new("history.browse", "Browse Files")
                .icon(LucideIcon::FolderTree)
                .on_click({
                    let this = this.clone();
                    move |_, _, cx| {
                        let _ = this.update(cx, |_, cx| {
                            cx.emit(HistoryEvent::BrowseFiles(browse.clone()));
                        });
                    }
                }),
            MenuItem::new("history.copy", "Copy Hash")
                .icon(LucideIcon::Copy)
                .on_click({
                    let hash = hash.clone();
                    move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))
                }),
            MenuItem::new("history.copy-patch", "Copy as Patch")
                .icon(LucideIcon::ClipboardCopy)
                .on_click(with_this(&this, {
                    let oid = oid.clone();
                    move |view, cx| view.copy_patch(oid.clone(), cx)
                })),
            MenuItem::new("history.save-patch", "Save as Patch…")
                .icon(LucideIcon::FileDown)
                .on_click(with_this(&this, {
                    let oid = oid.clone();
                    let summary = commit.summary.clone();
                    move |view, cx| view.save_patch(oid.clone(), &summary, cx)
                })),
            MenuItem::separator(),
            MenuItem::new("history.checkout", "Check Out (Detached)")
                .icon(LucideIcon::GitCommitHorizontal)
                .on_click(refs_do({
                    let hash = hash.clone();
                    Box::new(move |refs, cx| refs.checkout(Checkout::Detached(hash.clone()), cx))
                })),
            MenuItem::new("history.branch", "New Branch Here…")
                .icon(LucideIcon::GitBranchPlus)
                .on_click(refs_do({
                    let hash = hash.clone();
                    Box::new(move |refs, cx| {
                        refs.show(
                            RefDialog::CreateBranch {
                                start: Some(hash.clone()),
                            },
                            cx,
                        );
                    })
                })),
            MenuItem::new("history.tag", "New Tag Here…")
                .icon(LucideIcon::Tag)
                .on_click(refs_do({
                    let hash = hash.clone();
                    Box::new(move |refs, cx| {
                        refs.show(
                            RefDialog::CreateTag {
                                target: Some(hash.clone()),
                            },
                            cx,
                        );
                    })
                })),
        ];
        let bisect = |id: &'static str, label: &'static str, verdict: BisectVerdict| {
            let this = this.clone();
            let oid = oid.clone();
            MenuItem::new(id, label).on_click(move |_, _, cx| {
                let _ = this.update(cx, |_, cx| {
                    cx.emit(HistoryEvent::Bisect(oid.clone(), verdict));
                });
            })
        };
        items.push(
            MenuItem::submenu(
                "history.bisect",
                "Find a Bad Commit",
                vec![
                    bisect(
                        "history.bisect.bad",
                        "Mark as Bad (has the problem)",
                        BisectVerdict::Bad,
                    ),
                    bisect(
                        "history.bisect.good",
                        "Mark as Good (before the problem)",
                        BisectVerdict::Good,
                    ),
                ],
            )
            .icon(LucideIcon::Crosshair),
        );
        if let Some(current) = self.current_branch(cx) {
            let with_view = |f: ViewAction| {
                let this = this.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                    let _ = this.update(cx, |view, cx| f(view, cx));
                }
            };
            let reset = |id: &'static str, label: &'static str, mode: ResetMode| {
                let hash = hash.clone();
                MenuItem::new(id, label).on_click(refs_do(Box::new(move |refs, cx| {
                    refs.reset(hash.clone(), mode, cx);
                })))
            };
            items.extend([
                MenuItem::separator(),
                MenuItem::new("history.cherry-pick", format!("Cherry-pick onto {current}"))
                    .icon(LucideIcon::Cherry)
                    .disabled(commit.parents.len() > 1)
                    .on_click(with_view({
                        let oid = oid.clone();
                        Box::new(move |view, cx| view.cherry_pick(oid.clone(), cx))
                    })),
                MenuItem::new("history.revert", "Revert…")
                    .icon(LucideIcon::Undo2)
                    .disabled(commit.parents.len() > 1)
                    .on_click(with_view({
                        let oid = oid.clone();
                        Box::new(move |view, cx| view.revert(oid.clone(), cx))
                    })),
                MenuItem::submenu(
                    "history.reset",
                    format!("Reset {current} to Here"),
                    vec![
                        reset(
                            "history.reset.soft",
                            "Soft: keep changes staged",
                            ResetMode::Soft,
                        ),
                        reset(
                            "history.reset.mixed",
                            "Mixed: keep changes unstaged",
                            ResetMode::Mixed,
                        ),
                        MenuItem::new("history.reset.hard", "Hard: discard changes…").on_click(
                            refs_do({
                                let hash = hash.clone();
                                Box::new(move |refs, cx| {
                                    refs.show(
                                        RefDialog::ResetHard {
                                            target: hash.clone(),
                                        },
                                        cx,
                                    );
                                })
                            }),
                        ),
                    ],
                )
                .icon(LucideIcon::RotateCcw),
            ]);
            items.extend(self.edit_items(&commit, cx));
        }
        if let Some(web) = web {
            let url = web.commit(&hash);
            items.insert(
                2,
                MenuItem::new("history.open-web", web.open_label())
                    .icon(LucideIcon::ExternalLink)
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        self.menu.update(cx, |menu, cx| menu.set_items(items, cx));
    }

    fn ensure_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = &self.search_input {
            return input.clone();
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search messages, author:, path:, code: or a hash")
        });
        self.subscriptions
            .push(cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    // Wait for typing to pause before asking Git again.
                    this.tasks.push(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(300))
                            .await;
                        let _ = this.update(cx, |this, cx| {
                            let current = this
                                .search_input
                                .as_ref()
                                .map(|i| i.read(cx).value().to_string());
                            if current.as_deref() == Some(text.as_str()) {
                                this.set_search(&text, cx);
                            }
                        });
                    }));
                }
            }));
        self.search_input = Some(input.clone());
        input
    }

    /// `hidden` is how many ref labels did not fit on the row.
    fn render_row(
        commit: Option<&CommitInfo>,
        hidden: usize,
        theme: &UiTheme,
    ) -> gpui_kit::AnyElement {
        let Some(commit) = commit else {
            return div().into_any_element();
        };
        let c = &theme.colors;
        let when = u64::try_from(commit.author_time)
            .ok()
            .map(|s| std::time::UNIX_EPOCH + Duration::from_secs(s))
            .map(ago)
            .unwrap_or_default();
        let dark = theme.mode == crate::ui::theme::ThemeMode::Dark;
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .when(hidden > 0, |this| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .px(px(5.))
                        .rounded(px(8.))
                        .bg(c.muted)
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!("+{hidden}")),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(commit.summary.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(150.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(avatar(
                        &commit.author_name,
                        &commit.author_email,
                        px(18.),
                        dark,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(commit.author_name.clone()),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(84.))
                    .truncate()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(when),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(56.))
                    .font_family(theme.fonts.mono.clone())
                    .text_xs()
                    .text_color(c.muted_foreground.opacity(0.8))
                    .child(short(&commit.oid)),
            )
            .into_any_element()
    }
}

fn short(oid: &Oid) -> String {
    oid.as_str().chars().take(7).collect()
}

impl Render for HistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start(cx);
        let search = self.ensure_search(window, cx);
        if let Some(text) = self.search_field_text.take() {
            search.update(cx, |input, cx| input.set_value(text, window, cx));
        }
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let commits = self.commits.clone();
        let label_counts: Rc<HashMap<Oid, usize>> = Rc::new(
            self.labels
                .iter()
                .filter(|(_, labels)| labels.len() > MAX_LABELS)
                .map(|(oid, labels)| (oid.clone(), labels.len() - MAX_LABELS))
                .collect(),
        );
        let row_theme = theme.clone();
        let view = cx.entity().downgrade();
        let len = commits.len();
        let empty = if self.paging == Paging::Loading {
            "Reading the history…"
        } else if self.search.filter.is_empty() {
            "No commits yet."
        } else {
            "No commits match."
        };
        let graph = CommitGraph::new(
            "history.graph",
            "History",
            &self.graph,
            move |_, row: &ListRow, _, _| {
                let commit = commits.get(row.index);
                let hidden = commit
                    .and_then(|c| label_counts.get(&c.oid))
                    .copied()
                    .unwrap_or(0);
                Self::render_row(commit, hidden, &row_theme)
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
            let _ = view.update(cx, |view, cx| {
                if matches!(
                    event,
                    VirtualListEvent::SelectionChanged | VirtualListEvent::FocusChanged
                ) {
                    view.rebuild_menu(cx);
                    view.emit_selection(cx);
                    let focused = view.graph.focused();
                    let index = focused.and_then(|id| {
                        view.commits.iter().position(
                            |c| matches!(&id, ElementId::Name(n) if n.as_ref() == c.oid.as_str()),
                        )
                    });
                    if index.is_some_and(|i| i + PREFETCH >= len) {
                        view.load_more(cx);
                    }
                    cx.notify();
                }
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
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(c.border)
                    .child(
                        Menu::new(&self.scope_menu, "Show commits from")
                            .trigger_style_with(ghost_trigger(&theme, ButtonSize::Sm))
                            .trigger(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .max_w(px(180.))
                                    .child(
                                        gpui_icons::lucide(if self.scope == HistoryScope::All {
                                            LucideIcon::GitFork
                                        } else {
                                            LucideIcon::GitBranch
                                        })
                                        .size(px(14.))
                                        .flex_none()
                                        .text_color(c.foreground),
                                    )
                                    .child(div().min_w_0().truncate().child(self.scope.label()))
                                    .child(
                                        gpui_icons::lucide(LucideIcon::ChevronDown)
                                            .size(px(12.))
                                            .flex_none()
                                            .text_color(c.muted_foreground),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&search).aria_label("Search history")),
                    )
                    .child(
                        crate::workspace::title_bar::bar_button(
                            "history.head",
                            "Jump to HEAD",
                            false,
                            &theme,
                        )
                        .child(
                            gpui_icons::lucide(LucideIcon::LocateFixed)
                                .size(px(15.))
                                .text_color(c.foreground),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.reveal_head(cx))),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px(px(12.))
                        .py(px(6.))
                        .text_sm()
                        .text_color(c.destructive)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p(px(4.))
                    .child(context_menu(&self.menu, "Commit actions", graph).size_full()),
            )
            .when(self.paging == Paging::End && self.shallow, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(8.))
                        .py(px(6.))
                        .border_t_1()
                        .border_color(c.border)
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child("This clone has only recent history.")
                        .child(
                            Button::new("history.unshallow")
                                .variant(ButtonVariant::Ghost)
                                .label("Fetch Full History")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.fetch_full_history(cx);
                                })),
                        ),
                )
            })
            .when(self.paging != Paging::End && len > 0, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(8.))
                        .py(px(6.))
                        .border_t_1()
                        .border_color(c.border)
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!("{len} commits loaded"))
                        .child(
                            Button::new("history.more")
                                .variant(ButtonVariant::Ghost)
                                .label(if self.paging == Paging::Loading {
                                    "Loading…"
                                } else {
                                    "Load More"
                                })
                                .disabled(self.paging == Paging::Loading)
                                .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(oid: &str, parents: &[&str]) -> CommitInfo {
        CommitInfo {
            oid: Oid::new(oid),
            parents: parents.iter().map(|p| Oid::new(*p)).collect(),
            author_name: "Ada Lovelace".into(),
            author_email: "ada@example.com".into(),
            author_time: 0,
            summary: oid.into(),
        }
    }

    #[test]
    fn a_branch_scope_reads_the_branch_and_its_upstream() {
        use bonsai_git::{Branch, Tracking};
        let branch = |name: &str, upstream: Option<&str>, is_head: bool| Branch {
            name: name.into(),
            oid: Oid::new("a".repeat(40)),
            is_head,
            upstream: upstream.map(|u| Tracking {
                name: u.into(),
                gone: false,
                ahead: 0,
                behind: 0,
            }),
        };
        let snapshot = Snapshot {
            head: Head::Branch {
                name: "main".into(),
                oid: Some(Oid::new("a".repeat(40))),
            },
            changes: Vec::new(),
            branches: vec![
                branch("main", Some("origin/main"), true),
                branch("topic", None, false),
            ],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            remotes: Vec::new(),
            stashes: Vec::new(),
            worktrees: Vec::new(),
            submodules: Vec::new(),
            operation: None,
        };
        let refs = |list: &[&str]| LogScope::Refs(list.iter().map(|r| (*r).to_owned()).collect());
        assert_eq!(HistoryScope::All.log_scope(Some(&snapshot)), LogScope::All);
        assert_eq!(
            HistoryScope::Current.log_scope(Some(&snapshot)),
            refs(&["HEAD", "refs/remotes/origin/main"])
        );
        assert_eq!(
            HistoryScope::Branch("topic".into()).log_scope(Some(&snapshot)),
            refs(&["refs/heads/topic"])
        );
        assert_eq!(HistoryScope::Current.log_scope(None), LogScope::Head);
    }

    #[test]
    fn squashes_only_a_run_of_commits_into_the_oldest() {
        let (d, c, b, a) = (
            commit("d", &["c"]),
            commit("c", &["b"]),
            commit("b", &["a"]),
            commit("a", &["base"]),
        );
        let request = squash_request(&[&d, &c, &b]).unwrap();
        assert_eq!(request.base, Some(Oid::new("a")));
        assert_eq!(
            request.preset,
            [
                (Oid::new("d"), RebaseAction::Squash),
                (Oid::new("c"), RebaseAction::Squash)
            ]
        );
        // A gap, a merge or a single commit can't be squashed together.
        assert!(squash_request(&[&d, &b]).is_none());
        let merge = commit("m", &["d", "x"]);
        assert!(squash_request(&[&merge, &d]).is_none());
        assert!(squash_request(&[&a]).is_none());
    }

    #[test]
    fn names_patch_files_after_the_commit() {
        let oid = Oid::new("1a2b3c4d5e6f7a8b9c0d1a2b3c4d5e6f7a8b9c0d");
        assert_eq!(
            patch_file_name(&oid, "Fix the parser's “edge” case!"),
            "1a2b3c4-Fix-the-parser-s-edge-case.patch"
        );
        assert_eq!(patch_file_name(&oid, "…"), "1a2b3c4.patch");
    }

    #[test]
    fn reads_the_search_field() {
        assert_eq!(parse_search("  "), Search::default());
        assert_eq!(
            parse_search("fix crash").filter.message.as_deref(),
            Some("fix crash")
        );
        assert_eq!(
            parse_search("author: Grace").filter.author.as_deref(),
            Some("Grace")
        );
        assert_eq!(
            parse_search("path:src/ui").filter.path,
            Some(PathBuf::from("src/ui"))
        );
        assert_eq!(
            parse_search("code: retry(3) ").filter.code.as_deref(),
            Some("retry(3)")
        );
        let hash = parse_search("ABCDEF1");
        assert_eq!(hash.hash.as_deref(), Some("abcdef1"));
        assert!(hash.filter.is_empty());
        // Short or non-hex words are messages.
        assert!(parse_search("abc").hash.is_none());
        assert!(parse_search("deadbeefz").hash.is_none());
    }

    mod app {
        use bonsai_git::{Git, Repository};
        use gpui_kit::TestAppContext;

        use super::super::*;

        fn run(dir: &std::path::Path, args: &[&str]) -> String {
            let git = Git::locate(None).unwrap();
            let mut full = vec![
                "-c",
                "user.name=Ada",
                "-c",
                "user.email=ada@example.com",
                "-c",
                "commit.gpgsign=false",
            ];
            full.extend_from_slice(args);
            git.run(&git.command(full).cwd(dir)).unwrap().stdout_lossy()
        }

        #[gpui_kit::test]
        fn loads_labels_searches_and_cherry_picks(cx: &mut TestAppContext) {
            cx.update(crate::ui::theme::init);
            let dir = tempfile::tempdir().unwrap();
            let git = Git::locate(None).unwrap();
            let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
            std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
            run(dir.path(), &["add", "."]);
            run(dir.path(), &["commit", "-qm", "base"]);
            run(dir.path(), &["tag", "v1"]);
            run(dir.path(), &["switch", "-qc", "feature"]);
            std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
            run(dir.path(), &["add", "."]);
            run(dir.path(), &["commit", "-qm", "Add the b file"]);
            run(dir.path(), &["switch", "-q", "main"]);

            let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
            let refs = cx.new(|cx| RefsPanel::new(model.clone(), cx));
            let history = cx.new(|cx| HistoryView::new(model.clone(), refs, cx));
            cx.run_until_parked();
            history.update(cx, HistoryView::start);
            cx.run_until_parked();
            history.read_with(cx, |view, _| {
                let summaries: Vec<&str> =
                    view.commits().iter().map(|c| c.summary.as_str()).collect();
                assert_eq!(summaries, ["Add the b file", "base"]);
                assert!(view.is_exhausted());
                let base = view.graph().row(1).unwrap();
                let names: Vec<&str> = base.labels.iter().map(|l| l.text.as_ref()).collect();
                assert_eq!(names, ["main", "v1"]);
                assert_eq!(base.labels[0].kind, GraphLabelKind::Head);
            });

            history.update(cx, |view, cx| view.set_search("b file", cx));
            cx.run_until_parked();
            let feature = history.read_with(cx, |view, _| {
                assert_eq!(view.commits().len(), 1);
                view.commits()[0].oid.clone()
            });

            history.update(cx, |view, cx| {
                view.set_search("", cx);
            });
            cx.run_until_parked();
            history.update(cx, |view, cx| {
                assert!(view.reveal_hash(&feature.as_str()[..8], cx));
                assert_eq!(
                    view.selected().map(|c| c.oid.clone()),
                    Some(feature.clone())
                );
                view.cherry_pick(feature.clone(), cx);
            });
            cx.run_until_parked();
            assert!(dir.path().join("b.txt").exists());
            // main moved, so the history reloaded with the picked commit.
            history.read_with(cx, |view, _| {
                assert_eq!(view.commits().len(), 3);
                let picked: Vec<&CommitInfo> = view
                    .commits()
                    .iter()
                    .filter(|c| c.summary == "Add the b file" && c.oid != feature)
                    .collect();
                assert_eq!(picked.len(), 1);
                // The selection survived the reload.
                assert_eq!(view.selected().map(|c| c.oid.clone()), Some(feature));
            });
        }

        #[gpui_kit::test]
        fn showing_an_old_commit_pages_until_it_loads(cx: &mut TestAppContext) {
            use std::fmt::Write as _;

            cx.update(crate::ui::theme::init);
            let dir = tempfile::tempdir().unwrap();
            let git = Git::locate(None).unwrap();
            let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
            // 1,200 commits in one fast-import stream; each continues the branch.
            let mut stream = String::new();
            for n in 1..=1_200 {
                let message = format!("commit {n}");
                let _ = write!(
                    stream,
                    "commit refs/heads/main\ncommitter Ada <ada@example.com> {} +0000\ndata {}\n{message}\n",
                    1_700_000_000 + n,
                    message.len()
                );
                stream.push('\n');
            }
            git.run(
                &git.command(["fast-import", "--quiet"])
                    .cwd(dir.path())
                    .stdin(stream.into_bytes()),
            )
            .unwrap();
            let oldest = git
                .run(
                    &git.command(["rev-list", "--max-parents=0", "main"])
                        .cwd(dir.path()),
                )
                .unwrap()
                .stdout_lossy()
                .trim()
                .to_owned();

            let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
            let refs = cx.new(|cx| RefsPanel::new(model.clone(), cx));
            let history = cx.new(|cx| HistoryView::new(model.clone(), refs, cx));
            cx.run_until_parked();
            history.update(cx, |view, cx| view.reveal(&Oid::new(oldest.clone()), cx));
            cx.run_until_parked();
            history.read_with(cx, |view, _| {
                assert_eq!(view.commits().len(), 1_200);
                assert_eq!(
                    view.selected().map(|c| c.summary.as_str()),
                    Some("commit 1")
                );
            });
        }

        #[gpui_kit::test]
        fn a_shallow_clone_fetches_its_full_history(cx: &mut TestAppContext) {
            cx.update(crate::ui::theme::init);
            let dir = tempfile::tempdir().unwrap();
            let git = Git::locate(None).unwrap();
            let source = dir.path().join("source");
            Repository::init(&git, &source, Some("main")).unwrap();
            for n in 1..=3 {
                run(
                    &source,
                    &[
                        "commit",
                        "-q",
                        "--allow-empty",
                        "-m",
                        &format!("commit {n}"),
                    ],
                );
            }
            let options = bonsai_git::CloneOptions {
                shallow: true,
                submodules: false,
            };
            let repository = Repository::clone_with(
                &git,
                &format!("file://{}", source.display()),
                &dir.path().join("clone"),
                options,
                |_| {},
                None,
            )
            .unwrap();

            let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
            let refs = cx.new(|cx| RefsPanel::new(model.clone(), cx));
            let history = cx.new(|cx| HistoryView::new(model.clone(), refs, cx));
            cx.run_until_parked();
            history.update(cx, HistoryView::start);
            cx.run_until_parked();
            history.read_with(cx, |view, _| {
                assert!(view.is_shallow());
                assert_eq!(view.commits().len(), 1);
            });

            history.update(cx, HistoryView::fetch_full_history);
            cx.run_until_parked();
            history.read_with(cx, |view, _| {
                assert!(!view.is_shallow());
                assert_eq!(view.commits().len(), 3);
            });
        }
    }
}
