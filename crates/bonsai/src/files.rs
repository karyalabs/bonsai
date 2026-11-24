//! Files: every tracked file as a folder tree, a file's contents, and the
//! actions on a file (Blame, its History, Purge from History, Reveal).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use bonsai_git::{FileVersion, GrepResults, Oid};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::component::{ActiveTheme as _, Rope, highlighter::SyntaxHighlighter};
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, HighlightStyle,
    Image, ImageFormat, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _,
    Render, ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled,
    StyledImage as _, StyledText, Subscription, UniformListScrollHandle, WeakEntity, Window, div,
    img, prelude::FluentBuilder as _, px, uniform_list,
};

use crate::{
    blame_view::{BlameEvent, BlameView},
    file_icon::file_icon,
    repository::{RepositoryEvent, RepositoryModel, UndoHints},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        context_menu::context_menu,
        input::{Input, InputEvent, InputState},
        menu::{Menu, MenuItem, MenuState},
        theme::UiTheme,
        tooltip::text_tooltip,
        tree::{Tree, TreeEvent, TreeNode, TreeRow, TreeRowKind, TreeState},
    },
    workspace::title_bar::ghost_trigger,
};

/// Files larger than this show a note instead of their text.
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Larger files show without syntax colors, so parsing stays quick.
const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
/// A text row's height.
const ROW: f32 = 20.;
/// Matching lines kept from a search of the files' contents.
const MAX_HITS: usize = 2000;
/// A search result row's height.
const HIT_ROW: f32 = 24.;

/// What a search of the files' contents shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchStatus {
    Searching,
    Done(Arc<GrepResults>),
    Failed(SharedString),
}

/// A row of search results: a file, or one of its matching lines (an
/// index into the hits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultRow {
    File { first: usize, count: usize },
    Hit(usize),
}

fn result_rows(results: &GrepResults) -> Vec<ResultRow> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index < results.hits.len() {
        let path = &results.hits[index].path;
        let count = results.hits[index..]
            .iter()
            .take_while(|hit| &hit.path == path)
            .count();
        rows.push(ResultRow::File {
            first: index,
            count,
        });
        rows.extend((index..index + count).map(ResultRow::Hit));
        index += count;
    }
    rows
}

/// Where `query` appears in `text`, ignoring case when the query is all
/// lowercase, as the search does.
fn match_ranges(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let fold = !query.chars().any(char::is_uppercase);
    let (haystack, needle) = if fold {
        (text.to_ascii_lowercase(), query.to_ascii_lowercase())
    } else {
        (text.to_owned(), query.to_owned())
    };
    haystack
        .match_indices(&needle)
        .map(|(start, found)| start..start + found.len())
        .filter(|range| text.is_char_boundary(range.start) && text.is_char_boundary(range.end))
        .collect()
}

pub enum FilesEvent {
    /// Purge from History was chosen for this path.
    Purge(PathBuf),
    /// Show the commits that changed this path.
    History(PathBuf),
    /// Blame asked to show this commit in History.
    ShowCommit(Oid),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListStatus {
    /// Not read yet: the list loads when Files first opens.
    Idle,
    Loading,
    Ready,
    Failed(SharedString),
}

/// What the viewer shows for the selected file.
#[derive(Clone)]
pub enum Content {
    Loading,
    Text {
        lines: Arc<Vec<SharedString>>,
        bytes: u64,
    },
    Image {
        image: Arc<Image>,
        bytes: u64,
    },
    Binary {
        bytes: u64,
    },
    TooLarge {
        bytes: u64,
    },
    /// Tracked, but deleted in the work tree.
    Missing,
    Failed(SharedString),
}

/// A commit, branch or tag whose files Files shows instead of the work tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    /// What Git reads, such as a branch name or a commit hash.
    pub spec: String,
    /// What the header shows.
    pub label: SharedString,
}

pub struct FilesView {
    model: Entity<RepositoryModel>,
    status: ListStatus,
    /// `None` for the work tree.
    revision: Option<Revision>,
    revisions: Entity<MenuState>,
    paths: Arc<Vec<PathBuf>>,
    tree: TreeState,
    filter: Option<(Entity<InputState>, Subscription)>,
    filter_text: String,
    /// The box searches the files' contents instead of their names.
    search_contents: bool,
    search: Option<SearchStatus>,
    search_generation: u64,
    results_scroll: UniformListScrollHandle,
    /// The line a search result points at, highlighted in the viewer.
    goto_line: Option<usize>,
    /// The file the viewer shows.
    selected: Option<PathBuf>,
    content: Option<Content>,
    syntax: Option<Arc<Syntax>>,
    blame: Option<(Entity<BlameView>, Subscription)>,
    menu: Entity<MenuState>,
    scroll: UniformListScrollHandle,
    generation: u64,
    list_generation: u64,
    _events: Subscription,
}

impl EventEmitter<FilesEvent> for FilesView {}

/// The folder tree of `paths`: a folder's id is its path with a trailing
/// slash, a file's id is its path.
#[must_use]
pub fn file_tree(paths: &[PathBuf], filter: &str) -> Vec<TreeNode> {
    #[derive(Default)]
    struct Dir {
        dirs: BTreeMap<String, Dir>,
        files: Vec<(String, String)>,
    }
    fn nodes(prefix: &str, dir: Dir) -> Vec<TreeNode> {
        let mut out: Vec<TreeNode> = dir
            .dirs
            .into_iter()
            .map(|(name, child)| {
                let id = format!("{prefix}{name}/");
                let children = nodes(&id, child);
                TreeNode::folder(id, name, children)
            })
            .collect();
        out.extend(
            dir.files
                .into_iter()
                .map(|(id, name)| TreeNode::leaf(id, name)),
        );
        out
    }
    let needle = filter.trim().to_lowercase();
    let mut root = Dir::default();
    for path in paths {
        let text = path.to_string_lossy();
        if !needle.is_empty() && !text.to_lowercase().contains(&needle) {
            continue;
        }
        let mut parts: Vec<&str> = text.split('/').collect();
        let name = parts.pop().unwrap_or_default();
        let mut dir = &mut root;
        for part in parts {
            dir = dir.dirs.entry(part.to_owned()).or_default();
        }
        dir.files.push((text.clone().into_owned(), name.to_owned()));
    }
    nodes("", root)
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

fn image_format(path: &Path) -> Option<ImageFormat> {
    let extension = path.extension()?.to_string_lossy().to_lowercase();
    Some(match extension.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "gif" => ImageFormat::Gif,
        "svg" => ImageFormat::Svg,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "ico" => ImageFormat::Ico,
        _ => return None,
    })
}

/// Reads a work tree file for the viewer.
#[must_use]
pub fn read_content(absolute: &Path) -> Content {
    let metadata = match std::fs::metadata(absolute) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Content::Missing,
        Err(error) => return Content::Failed(error.to_string().into()),
    };
    let bytes = metadata.len();
    if bytes > MAX_BYTES {
        return Content::TooLarge { bytes };
    }
    match std::fs::read(absolute) {
        Ok(data) => classify(absolute, data),
        Err(error) => Content::Failed(error.to_string().into()),
    }
}

/// What `data`, the contents of `path`, shows as.
#[must_use]
pub fn classify(path: &Path, data: Vec<u8>) -> Content {
    let bytes = data.len() as u64;
    if bytes > MAX_BYTES {
        return Content::TooLarge { bytes };
    }
    if let Some(format) = image_format(path) {
        return Content::Image {
            image: Arc::new(Image::from_bytes(format, data)),
            bytes,
        };
    }
    // Git's own test: a NUL in the first 8000 bytes means binary.
    if data.iter().take(8000).any(|&b| b == 0) {
        return Content::Binary { bytes };
    }
    let text = String::from_utf8_lossy(&data);
    let lines = text
        .lines()
        .map(|line| SharedString::from(line.replace('\t', "    ")))
        .collect();
    Content::Text {
        lines: Arc::new(lines),
        bytes,
    }
}

/// "12.3 KB".
#[must_use]
pub fn byte_size(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if value < 1024. * 1024. {
        format!("{:.1} KB", value / 1024.)
    } else {
        format!("{:.1} MB", value / 1024. / 1024.)
    }
}

/// A parsed file, colored line by line as rows show.
pub struct Syntax {
    highlighter: SyntaxHighlighter,
    /// Where each line starts in the parsed text.
    starts: Vec<usize>,
}

/// The grammar for `path`, by its name or extension.
#[must_use]
pub fn language(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match name.as_str() {
        "makefile" | "gnumakefile" => return "make",
        "cmakelists.txt" => return "cmake",
        _ => {}
    }
    match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("rs") => "rust",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "tsx",
        Some("py" | "pyi") => "python",
        Some("rb") => "ruby",
        Some("go") => "go",
        Some("swift") => "swift",
        Some("c" | "h") => "c",
        Some("cc" | "cpp" | "cxx" | "hpp" | "hxx") => "cpp",
        Some("cs") => "csharp",
        Some("java") => "java",
        Some("kt" | "kts") => "kotlin",
        Some("scala") => "scala",
        Some("css" | "scss") => "css",
        Some("html" | "htm") => "html",
        Some("json" | "jsonc") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("md" | "mdx") => "markdown",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("sql") => "sql",
        Some("lua") => "lua",
        Some("php") => "php",
        Some("ex" | "exs") => "elixir",
        Some("zig") => "zig",
        Some("proto") => "proto",
        Some("graphql" | "gql") => "graphql",
        Some("svelte") => "svelte",
        Some("astro") => "astro",
        Some("diff" | "patch") => "diff",
        _ => "text",
    }
}

/// Parses `lines` as `path`'s language, or `None` for plain text and large
/// files.
#[must_use]
pub fn parse_syntax(path: &Path, lines: &[SharedString]) -> Option<Syntax> {
    let language = language(path);
    if language == "text" {
        return None;
    }
    let mut text = String::new();
    let mut starts = Vec::with_capacity(lines.len());
    for line in lines {
        starts.push(text.len());
        text.push_str(line);
        text.push('\n');
        if text.len() > MAX_HIGHLIGHT_BYTES {
            return None;
        }
    }
    let mut highlighter = SyntaxHighlighter::new(language);
    highlighter.update(None, &Rope::from(text.as_str()), None);
    Some(Syntax {
        highlighter,
        starts,
    })
}

impl FilesView {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            if !matches!(event, RepositoryEvent::SnapshotChanged) {
                return;
            }
            this.rebuild_revisions(cx);
            if this.status != ListStatus::Idle {
                this.load(cx);
                this.reload_content(cx);
            }
        });
        let tree = TreeState::new(Vec::new(), []).expect("an empty tree has no duplicates");
        Self {
            model,
            status: ListStatus::Idle,
            revision: None,
            revisions: cx.new(|cx| MenuState::new(Vec::new(), cx).width(px(260.))),
            paths: Arc::default(),
            tree,
            filter: None,
            filter_text: String::new(),
            search_contents: false,
            search: None,
            search_generation: 0,
            results_scroll: UniformListScrollHandle::new(),
            goto_line: None,
            selected: None,
            content: None,
            syntax: None,
            blame: None,
            menu: cx.new(|cx| MenuState::new(Vec::new(), cx)),
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            list_generation: 0,
            _events: events,
        }
    }

    /// Reads the file list the first time Files opens.
    pub fn activate(&mut self, cx: &mut Context<Self>) {
        if self.status == ListStatus::Idle {
            self.rebuild_revisions(cx);
            self.load(cx);
        }
    }

    #[must_use]
    pub fn status(&self) -> &ListStatus {
        &self.status
    }

    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    #[must_use]
    pub fn tree(&self) -> &TreeState {
        &self.tree
    }

    #[must_use]
    pub fn selected(&self) -> Option<&PathBuf> {
        self.selected.as_ref()
    }

    #[must_use]
    pub fn content(&self) -> Option<&Content> {
        self.content.as_ref()
    }

    #[must_use]
    pub fn blame_view(&self) -> Option<&Entity<BlameView>> {
        self.blame.as_ref().map(|(view, _)| view)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.list_generation += 1;
        let generation = self.list_generation;
        if self.status != ListStatus::Ready {
            self.status = ListStatus::Loading;
        }
        let repository = self.model.read(cx).repository().clone();
        let rev = self.revision.as_ref().map(|r| r.spec.clone());
        let task = cx.background_spawn(async move { repository.files_at(rev.as_deref()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.list_generation != generation {
                    return;
                }
                match result {
                    Ok(paths) => {
                        this.paths = Arc::new(paths);
                        this.status = ListStatus::Ready;
                        this.sync(cx);
                        if this.search_contents {
                            this.run_search(cx);
                        }
                    }
                    Err(error) => this.status = ListStatus::Failed(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Rebuilds the tree from the paths and the filter.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let nodes = file_tree(&self.paths, &self.filter_text);
        let mut folders = Vec::new();
        if !self.filter_text.trim().is_empty() {
            folder_ids(&nodes, &mut folders);
        } else if self.tree.visible_len() == 0 {
            // Open the top folders when there are only a few.
            if nodes.len() <= 3 {
                folders.extend(nodes.iter().map(|node| node.id.clone()));
            }
        }
        let _ = self.tree.replace_nodes(nodes);
        self.tree.expand_folders(folders);
        cx.notify();
    }

    /// Shows only the paths that contain `text`.
    pub fn set_filter(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.filter_text != text {
            text.clone_into(&mut self.filter_text);
            self.sync(cx);
        }
    }

    #[must_use]
    pub fn search(&self) -> Option<&SearchStatus> {
        self.search.as_ref()
    }

    /// Switches the box between filtering names and searching contents.
    pub fn set_search_contents(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_contents == on {
            return;
        }
        self.search_contents = on;
        let filter = self.ensure_filter(window, cx);
        filter.update(cx, |input, cx| {
            input.set_placeholder(
                if on {
                    "Search file contents"
                } else {
                    "Filter files"
                },
                window,
                cx,
            );
            input.focus(window, cx);
        });
        if on {
            // The names stay unfiltered while the box searches contents.
            self.set_filter_text(String::new(), cx);
            self.run_search(cx);
        } else {
            self.search = None;
            self.goto_line = None;
            let text = filter.read(cx).value().to_string();
            self.set_filter_text(text, cx);
        }
        cx.notify();
    }

    fn set_filter_text(&mut self, text: String, cx: &mut Context<Self>) {
        if self.filter_text != text {
            self.filter_text = text;
            self.sync(cx);
        }
    }

    /// The text in the box.
    fn query(&self, cx: &gpui_kit::App) -> String {
        self.filter
            .as_ref()
            .map(|(input, _)| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Searches the files' contents for the text in the box.
    pub fn run_search(&mut self, cx: &mut Context<Self>) {
        self.search_generation += 1;
        let generation = self.search_generation;
        let query = self.query(cx);
        if query.trim().is_empty() {
            self.search = None;
            cx.notify();
            return;
        }
        self.search = Some(SearchStatus::Searching);
        let repository = self.model.read(cx).repository().clone();
        let rev = self.revision.as_ref().map(|r| r.spec.clone());
        let task =
            cx.background_spawn(async move { repository.grep(&query, rev.as_deref(), MAX_HITS) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.search = Some(match result {
                    Ok(results) => SearchStatus::Done(Arc::new(results)),
                    Err(error) => SearchStatus::Failed(error.to_string().into()),
                });
                this.results_scroll = UniformListScrollHandle::new();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Shows a search result's file in the viewer at its line.
    pub fn open_hit(&mut self, path: &Path, line: usize, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(path) {
            self.goto_line = Some(line);
            self.scroll_to_goto_line();
            cx.notify();
        } else {
            self.reveal(path, cx);
            // After revealing, which clears it; the line scrolls into view
            // once the file is read.
            self.goto_line = Some(line);
        }
    }

    fn scroll_to_goto_line(&self) {
        if let Some(line) = self.goto_line {
            self.scroll
                .scroll_to_item(line.saturating_sub(1), ScrollStrategy::Center);
        }
    }

    fn ensure_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((input, _)) = &self.filter {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                if this.search_contents {
                    this.run_search(cx);
                } else {
                    let text = input.read(cx).value().to_string();
                    this.set_filter(&text, cx);
                }
            }
        });
        self.filter = Some((input.clone(), subscription));
        input
    }

    /// Shows `path` in the viewer, or nothing for `None` or a folder.
    pub fn select(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.selected == path {
            return;
        }
        self.selected = path;
        self.goto_line = None;
        self.blame = None;
        self.scroll = UniformListScrollHandle::new();
        self.content = None;
        self.syntax = None;
        self.reload_content(cx);
        self.rebuild_menu(cx);
    }

    /// Selects `path` in the tree, opening its folders, and shows it.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let id: SharedString = path.to_string_lossy().into_owned().into();
        self.tree.reveal(&id);
        self.tree.set_selected([id]);
        self.select(Some(path.to_path_buf()), cx);
    }

    fn reload_content(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let Some(path) = self.selected.clone() else {
            self.content = None;
            cx.notify();
            return;
        };
        if self.content.is_none() {
            self.content = Some(Content::Loading);
        }
        let repository = self.model.read(cx).repository().clone();
        let absolute = repository.root().join(&path);
        let rev = self.revision.as_ref().map(|r| r.spec.clone());
        let task = cx.background_spawn(async move {
            let content = match rev {
                None => read_content(&absolute),
                Some(rev) => match repository.file_version(&path, &FileVersion::Commit(rev)) {
                    Ok(Some(data)) => classify(&absolute, data),
                    Ok(None) => Content::Missing,
                    Err(error) => Content::Failed(error.to_string().into()),
                },
            };
            let syntax = match &content {
                Content::Text { lines, .. } => parse_syntax(&absolute, lines).map(Arc::new),
                _ => None,
            };
            (content, syntax)
        });
        cx.spawn(async move |this, cx| {
            let (content, syntax) = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.content = Some(content);
                    this.syntax = syntax;
                    this.scroll_to_goto_line();
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn focused_path(&self) -> Option<PathBuf> {
        let id = self
            .tree
            .focused()
            .or_else(|| self.tree.selected().into_iter().next())?;
        (!id.ends_with('/')).then(|| PathBuf::from(id.as_ref()))
    }

    /// Opens Blame for the shown file, or closes it.
    pub fn toggle_blame(&mut self, cx: &mut Context<Self>) {
        if self.blame.take().is_some() {
            cx.notify();
            return;
        }
        let Some(path) = self.selected.clone() else {
            return;
        };
        let model = self.model.clone();
        let rev = self.revision.as_ref().map(|r| Oid::new(r.spec.clone()));
        let view = cx.new(|cx| BlameView::new(&model, path, rev, cx).close_label("Show File"));
        let events = cx.subscribe(&view, |this, _, event: &BlameEvent, cx| match event {
            BlameEvent::Close => {
                this.blame = None;
                cx.notify();
            }
            BlameEvent::ShowCommit(oid) => cx.emit(FilesEvent::ShowCommit(oid.clone())),
        });
        self.blame = Some((view, events));
        cx.notify();
    }

    fn actions(&self, cx: &Context<Self>) -> Vec<MenuItem> {
        let Some(path) = self.selected.clone() else {
            return Vec::new();
        };
        let this = cx.entity().downgrade();
        let absolute = self.model.read(cx).repository().root().join(&path);
        let emit = |this: &WeakEntity<Self>, event: fn(PathBuf) -> FilesEvent, path: &PathBuf| {
            let this = this.clone();
            let path = path.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                let _ = this.update(cx, |_, cx| cx.emit(event(path.clone())));
            }
        };
        let copy = path.to_string_lossy().into_owned();
        let reveal = absolute.clone();
        let at_work_tree = self.revision.is_none();
        let mut items = vec![
            MenuItem::new("files.blame", "Blame")
                .icon(LucideIcon::ScrollText)
                .on_click({
                    let this = this.clone();
                    move |_, _, cx| {
                        let _ = this.update(cx, Self::toggle_blame);
                    }
                }),
            MenuItem::new("files.history", "Show History")
                .icon(LucideIcon::FileClock)
                .on_click(emit(&this, FilesEvent::History, &path)),
            MenuItem::new("files.copy-path", "Copy Path")
                .icon(LucideIcon::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                }),
        ];
        if at_work_tree {
            let untrack = path.clone();
            items.push(
                MenuItem::new("files.stop-tracking", "Stop Tracking and Ignore")
                    .icon(LucideIcon::EyeOff)
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            let path = untrack.clone();
                            let _ = this.update(cx, |view, cx| view.stop_tracking(path, cx));
                        }
                    }),
            );
            items.push(
                MenuItem::new("files.reveal", "Reveal in Finder")
                    .icon(LucideIcon::FolderOpen)
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
            );
            items.push(
                MenuItem::new("files.open", "Open in Default App")
                    .icon(LucideIcon::ExternalLink)
                    .on_click(move |_, _, cx| cx.open_with_system(&absolute)),
            );
        }
        if let Some(revision) = &self.revision {
            let spec = revision.spec.clone();
            let restore = path.clone();
            items.push(
                MenuItem::new("files.restore", "Restore This Version")
                    .icon(LucideIcon::ArchiveRestore)
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            let (spec, path) = (spec.clone(), restore.clone());
                            let _ = this.update(cx, |view, cx| view.restore(spec, path, cx));
                        }
                    }),
            );
        }
        let snapshot = self.model.read(cx).snapshot();
        let web = snapshot.and_then(|s| crate::web::repository_web(s, cx));
        let revision = match &self.revision {
            Some(revision) => Some(revision.spec.clone()),
            None => snapshot.and_then(crate::web::work_tree_revision),
        };
        if let (Some(web), Some(revision)) = (web, revision) {
            let url = web.file(&revision, &path.to_string_lossy(), None);
            items.push(
                MenuItem::new("files.open-web", web.open_label())
                    .icon(LucideIcon::Globe)
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        items.push(
            MenuItem::new("files.purge", "Purge from History…")
                .icon(LucideIcon::Eraser)
                .on_click(emit(&this, FilesEvent::Purge, &path)),
        );
        items
    }

    /// Removes `path` from the index and adds it to `.gitignore`, keeping
    /// the file. Changes shows the staged removal, ready to commit.
    pub fn stop_tracking(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = format!("Stop tracking {}", path.to_string_lossy());
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.stop_tracking(&[path]),
                cx,
            );
        });
    }

    /// Puts `path` in the work tree back to how `revision` has it. The
    /// current file is saved first, so ⌘Z brings it back.
    pub fn restore(&mut self, revision: String, path: PathBuf, cx: &mut Context<Self>) {
        let name = format!("Restore {}", path.to_string_lossy());
        let hints = UndoHints {
            backup: vec![path.clone()],
            reset: None,
        };
        self.model.update(cx, |model, cx| {
            model.run_with(
                name,
                hints,
                move |repository| repository.restore_from(&revision, &[path]),
                cx,
            );
        });
    }

    #[must_use]
    pub fn revision(&self) -> Option<&Revision> {
        self.revision.as_ref()
    }

    /// Shows `revision`'s files, or the work tree's for `None`, keeping
    /// the selected path when it is there too.
    pub fn set_revision(&mut self, revision: Option<Revision>, cx: &mut Context<Self>) {
        if self.revision == revision {
            self.activate(cx);
            return;
        }
        self.revision = revision;
        self.status = ListStatus::Loading;
        self.blame = None;
        self.content = None;
        self.syntax = None;
        self.load(cx);
        self.reload_content(cx);
        self.rebuild_menu(cx);
        self.rebuild_revisions(cx);
    }

    /// The revision menu: the work tree, then branches and tags.
    fn rebuild_revisions(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let pick = |revision: Option<Revision>| {
            let this = this.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                let revision = revision.clone();
                let _ = this.update(cx, |view, cx| view.set_revision(revision, cx));
            }
        };
        let current = self.revision.as_ref().map(|r| r.spec.clone());
        let mut items = vec![
            MenuItem::new("files.rev.work-tree", "Work Tree")
                .icon(LucideIcon::HardDrive)
                .checked(current.is_none())
                .on_click(pick(None)),
        ];
        if let Some(snapshot) = self.model.read(cx).snapshot() {
            if !snapshot.branches.is_empty() {
                items.push(MenuItem::separator());
                items.push(MenuItem::label("files.rev.branches", "Branches"));
            }
            for branch in &snapshot.branches {
                items.push(
                    MenuItem::new(
                        SharedString::from(format!("files.rev.branch.{}", branch.name)),
                        branch.name.clone(),
                    )
                    .icon(LucideIcon::GitBranch)
                    .checked(current.as_deref() == Some(branch.name.as_str()))
                    .on_click(pick(Some(Revision {
                        spec: branch.name.clone(),
                        label: branch.name.clone().into(),
                    }))),
                );
            }
            if !snapshot.tags.is_empty() {
                items.push(MenuItem::separator());
                items.push(MenuItem::label("files.rev.tags", "Tags"));
            }
            for tag in &snapshot.tags {
                let spec = format!("refs/tags/{}", tag.name);
                items.push(
                    MenuItem::new(
                        SharedString::from(format!("files.rev.tag.{}", tag.name)),
                        tag.name.clone(),
                    )
                    .icon(LucideIcon::Tag)
                    .checked(current.as_deref() == Some(spec.as_str()))
                    .on_click(pick(Some(Revision {
                        spec,
                        label: tag.name.clone().into(),
                    }))),
                );
            }
        }
        self.revisions
            .update(cx, |menu, cx| menu.set_items(items, cx));
    }

    fn rebuild_menu(&mut self, cx: &mut Context<Self>) {
        let items = self.actions(cx);
        self.menu.update(cx, |menu, cx| menu.set_items(items, cx));
    }

    fn render_results(&self, theme: &UiTheme, cx: &mut Context<Self>) -> AnyElement {
        let c = &theme.colors;
        let note = |text: SharedString, color| {
            div()
                .p(px(16.))
                .text_sm()
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        let results = match &self.search {
            None => {
                return note(
                    "Type to search the text of every tracked file.".into(),
                    c.muted_foreground,
                );
            }
            Some(SearchStatus::Searching) => {
                return note("Searching…".into(), c.muted_foreground);
            }
            Some(SearchStatus::Failed(error)) => return note(error.clone(), c.destructive),
            Some(SearchStatus::Done(results)) if results.hits.is_empty() => {
                return note("No file contains this text.".into(), c.muted_foreground);
            }
            Some(SearchStatus::Done(results)) => results.clone(),
        };
        let rows = Arc::new(result_rows(&results));
        let query = self.query(cx);
        let mono = theme.fonts.mono.clone();
        let muted = c.muted_foreground;
        let hover = c.accent;
        let found = HighlightStyle {
            background_color: Some(gpui_kit::Hsla::from(c.warning.opacity(0.28))),
            ..HighlightStyle::default()
        };
        let selected = self.selected.clone();
        let goto = self.goto_line;
        let selected_bg = c.accent;
        let view = cx.entity().downgrade();
        let count = rows.len();
        uniform_list("files.results", count, move |range, _, _| {
            range
                .map(|index| match rows[index] {
                    ResultRow::File { first, count } => {
                        let path = results.hits[first].path.clone();
                        let name = path.file_name().map_or_else(
                            || path.to_string_lossy().into_owned(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        let folder = path
                            .parent()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        div()
                            .id(("files.result", index))
                            .h(px(HIT_ROW))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_sm()
                            .child(
                                lucide(file_icon(&path))
                                    .size(px(14.))
                                    .flex_none()
                                    .text_color(muted),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(name),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(folder),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(count.to_string()),
                            )
                            .into_any_element()
                    }
                    ResultRow::Hit(hit) => {
                        let hit = results.hits[hit].clone();
                        let text = hit.text.trim_start().to_owned();
                        let highlights: Vec<_> = match_ranges(&text, &query)
                            .into_iter()
                            .map(|range| (range, found))
                            .collect();
                        let is_selected =
                            selected.as_ref() == Some(&hit.path) && goto == Some(hit.line);
                        let view = view.clone();
                        div()
                            .id(("files.result", index))
                            .w_full()
                            .h(px(HIT_ROW))
                            .pl(px(32.))
                            .pr(px(12.))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .cursor_pointer()
                            .when(is_selected, |row| row.bg(selected_bg))
                            .hover(move |row| row.bg(hover))
                            .child(
                                div()
                                    .w(px(36.))
                                    .flex_none()
                                    .flex()
                                    .justify_end()
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(muted)
                                    .child(hit.line.to_string()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .child(StyledText::new(text).with_highlights(highlights)),
                            )
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |view, cx| {
                                    view.open_hit(&hit.path, hit.line, cx);
                                });
                            })
                            .into_any_element()
                    }
                })
                .collect()
        })
        .track_scroll(&self.results_scroll)
        .size_full()
        .py(px(4.))
        .into_any_element()
    }

    fn render_tree(&self, theme: &UiTheme, cx: &mut Context<Self>) -> Tree {
        let row_theme = theme.clone();
        let view = cx.entity().downgrade();
        Tree::new(
            "files.tree",
            "Files",
            self.tree.clone(),
            move |row: &TreeRow, _, _| {
                let c = &row_theme.colors;
                let folder = matches!(row.kind, TreeRowKind::Folder { .. });
                let icon = if folder {
                    LucideIcon::Folder
                } else {
                    file_icon(Path::new(row.id.as_ref()))
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        lucide(icon)
                            .size(px(14.))
                            .flex_none()
                            .text_color(c.muted_foreground),
                    )
                    .child(div().min_w_0().truncate().child(row.label.clone()))
                    .into_any_element()
            },
        )
        .empty(
            div()
                .p(px(16.))
                .text_sm()
                .text_color(theme.colors.muted_foreground)
                .child(if self.filter_text.trim().is_empty() {
                    "No tracked files yet."
                } else {
                    "No files match the filter."
                }),
        )
        .on_event(move |event, _, cx| {
            let _ = view.update(cx, |view, cx| match event {
                TreeEvent::SelectionChanged | TreeEvent::FocusChanged => {
                    if let Some(path) = view.focused_path() {
                        view.select(Some(path), cx);
                    }
                    cx.notify();
                }
                TreeEvent::Activate(id) if !id.ends_with('/') => {
                    view.select(Some(PathBuf::from(id.as_ref())), cx);
                }
                _ => {}
            });
        })
    }

    /// The viewer for the selected file, for the detail pane.
    pub fn render_detail(view: &Entity<Self>, cx: &mut gpui_kit::App) -> AnyElement {
        view.update(cx, Self::render_viewer)
    }

    fn render_viewer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let Some(path) = self.selected.clone() else {
            return message(
                LucideIcon::FolderTree,
                "No file selected",
                "Choose a file to see what it holds.",
                &theme,
            );
        };
        if let Some((blame, _)) = &self.blame {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.render_header(&path, &theme, cx))
                .child(div().flex_1().min_h_0().child(blame.clone()))
                .into_any_element();
        }
        let body = match self.content.clone() {
            None | Some(Content::Loading) => div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Reading the file…")
                .into_any_element(),
            Some(Content::Text { lines, .. }) if lines.is_empty() => message(
                LucideIcon::File,
                "Empty file",
                "This file has no text.",
                &theme,
            ),
            Some(Content::Text { lines, .. }) => self.render_text(lines, &theme, cx),
            Some(Content::Image { image, .. }) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .bg(c.muted.opacity(0.4))
                .child(
                    img(image)
                        .max_w_full()
                        .max_h_full()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            Some(Content::Binary { bytes }) => message(
                LucideIcon::FileDigit,
                "Binary file",
                &format!("{} of binary data.", byte_size(bytes)),
                &theme,
            ),
            Some(Content::TooLarge { bytes }) => message(
                LucideIcon::FileExclamationPoint,
                "Large file",
                &format!(
                    "This file is {}. Open it in its default app to read it.",
                    byte_size(bytes)
                ),
                &theme,
            ),
            Some(Content::Missing) if self.revision.is_some() => message(
                LucideIcon::FileX,
                "Not in this revision",
                "This file isn't in the chosen branch, tag or commit.",
                &theme,
            ),
            Some(Content::Missing) => message(
                LucideIcon::FileX,
                "Deleted in the work tree",
                "Git still tracks this file, but it is not on disk. Changes shows the deletion.",
                &theme,
            ),
            Some(Content::Failed(error)) => message(
                LucideIcon::TriangleAlert,
                "Bonsai could not read this file",
                &error,
                &theme,
            ),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_header(&path, &theme, cx))
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn render_header(
        &self,
        path: &Path,
        theme: &UiTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = &theme.colors;
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let folder = path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| !p.is_empty());
        let facts = match &self.content {
            Some(Content::Text { lines, bytes }) => Some(format!(
                "{} {} · {}",
                lines.len(),
                if lines.len() == 1 { "line" } else { "lines" },
                byte_size(*bytes)
            )),
            Some(
                Content::Image { bytes, .. }
                | Content::Binary { bytes }
                | Content::TooLarge { bytes },
            ) => Some(byte_size(*bytes)),
            _ => None,
        };
        let button = |id: &'static str, icon: LucideIcon, label: &'static str| {
            Button::new(id)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Sm)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(lucide(icon).size(px(14.)).text_color(c.foreground))
                        .child(label),
                )
        };
        let history_path = path.to_path_buf();
        let purge_path = path.to_path_buf();
        let copy = path.to_string_lossy().into_owned();
        let reveal = self.model.read(cx).repository().root().join(path);
        let blaming = self.blame.is_some();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(44.))
            .px(px(12.))
            .border_b_1()
            .border_color(c.border)
            .child(
                lucide(file_icon(path))
                    .size(px(15.))
                    .flex_none()
                    .text_color(c.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .child(
                        div()
                            .min_w_0()
                            .flex_shrink_0()
                            .max_w_full()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(name),
                    )
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
                    .when_some(facts, |this, facts| {
                        this.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(facts),
                        )
                    }),
            )
            .child(
                button(
                    "files.header.blame",
                    LucideIcon::ScrollText,
                    if blaming { "Show File" } else { "Blame" },
                )
                .on_click(cx.listener(|view, _, _, cx| view.toggle_blame(cx))),
            )
            .child(
                button("files.header.history", LucideIcon::FileClock, "History").on_click(
                    cx.listener(move |_, _, _, cx| {
                        cx.emit(FilesEvent::History(history_path.clone()));
                    }),
                ),
            )
            .child(
                button("files.header.purge", LucideIcon::Eraser, "Purge…").on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(FilesEvent::Purge(purge_path.clone()))),
                ),
            )
            .child(
                icon_button("files.header.copy", LucideIcon::Copy, "Copy Path", theme).on_click(
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    },
                ),
            )
            .when(self.revision.is_none(), |this| {
                this.child(
                    icon_button(
                        "files.header.reveal",
                        LucideIcon::FolderOpen,
                        "Reveal in Finder",
                        theme,
                    )
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                )
            })
    }

    fn render_text(
        &self,
        lines: Arc<Vec<SharedString>>,
        theme: &UiTheme,
        _: &mut Context<Self>,
    ) -> AnyElement {
        let digits = u16::try_from(lines.len().to_string().len()).unwrap_or(6);
        let mono = theme.fonts.mono.clone();
        let muted = theme.colors.muted_foreground;
        let count = lines.len();
        let syntax = self.syntax.clone();
        let target = self.goto_line.map(|line| line.saturating_sub(1));
        let target_bg = theme.colors.warning.opacity(0.16);
        uniform_list("files.text", count, move |range, _, cx| {
            let colors = cx.theme().highlight_theme.clone();
            range
                .map(|index| {
                    let line = lines[index].clone();
                    let text = match &syntax {
                        Some(syntax) => {
                            let start = syntax.starts[index];
                            let highlights = syntax
                                .highlighter
                                .styles(&(start..start + line.len()), colors.as_ref())
                                .into_iter()
                                .map(|(range, style)| {
                                    (range.start - start..range.end - start, style)
                                })
                                .collect::<Vec<_>>();
                            StyledText::new(line).with_highlights(highlights)
                        }
                        None => StyledText::new(line),
                    };
                    div()
                        .w_full()
                        .h(px(ROW))
                        .flex()
                        .items_center()
                        .when(target == Some(index), |row| row.bg(target_bg))
                        .child(
                            div()
                                .w(px(8. * f32::from(digits) + 24.))
                                .flex_none()
                                .pr(px(12.))
                                .flex()
                                .justify_end()
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(muted)
                                .child((index + 1).to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_xs()
                                .font_family(mono.clone())
                                .child(text),
                        )
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        .size_full()
        .py(px(6.))
        .into_any_element()
    }
}

/// A ghost icon button with a tooltip.
fn icon_button(id: &'static str, icon: LucideIcon, tip: &'static str, theme: &UiTheme) -> Button {
    toggle_button(id, icon, tip, false, theme)
}

/// An icon button that shows whether it's on.
fn toggle_button(
    id: &'static str,
    icon: LucideIcon,
    tip: &'static str,
    on: bool,
    theme: &UiTheme,
) -> Button {
    let style = theme.clone();
    Button::new(id)
        .aria_label(tip)
        .style_with(move |button| {
            crate::ui::button::style_button(
                button,
                false,
                if on {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                },
                ButtonSize::IconSm,
                &style,
            )
            .tooltip(move |_, cx| text_tooltip(tip.into(), cx))
        })
        .child(
            lucide(icon)
                .size(px(14.))
                .text_color(theme.colors.foreground),
        )
}

fn message(icon: LucideIcon, title: &str, detail: &str, theme: &UiTheme) -> AnyElement {
    let c = &theme.colors;
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .p(px(24.))
        .child(lucide(icon).size(px(22.)).text_color(c.muted_foreground))
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(title.to_owned()),
        )
        .child(
            div()
                .max_w(px(360.))
                .text_center()
                .text_sm()
                .text_color(c.muted_foreground)
                .child(detail.to_owned()),
        )
        .into_any_element()
}

impl Render for FilesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let filter = self.ensure_filter(window, cx);
        let revision_label: SharedString = self
            .revision
            .as_ref()
            .map_or_else(|| "Work Tree".into(), |r| r.label.clone());
        let at_revision = self.revision.is_some();
        let body = match &self.status {
            ListStatus::Idle | ListStatus::Loading => div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Listing the files…")
                .into_any_element(),
            ListStatus::Failed(error) => div()
                .p(px(16.))
                .text_sm()
                .text_color(c.destructive)
                .child(error.clone())
                .into_any_element(),
            ListStatus::Ready if self.search_contents => self.render_results(&theme, cx),
            ListStatus::Ready => {
                context_menu(&self.menu, "File actions", self.render_tree(&theme, cx))
                    .size_full()
                    .into_any_element()
            }
        };
        let count = self.paths.len();
        let search_contents = self.search_contents;
        let found = match &self.search {
            Some(SearchStatus::Done(results)) if search_contents && !results.hits.is_empty() => {
                Some((
                    results.hits.len(),
                    results.truncated,
                    results
                        .hits
                        .iter()
                        .map(|hit| &hit.path)
                        .collect::<std::collections::BTreeSet<_>>()
                        .len(),
                ))
            }
            _ => None,
        };
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
                        Menu::new(&self.revisions, "Show files from")
                            .trigger_style_with(ghost_trigger(&theme, ButtonSize::Sm))
                            .trigger(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .max_w(px(160.))
                                    .child(
                                        lucide(if at_revision {
                                            LucideIcon::GitCommitHorizontal
                                        } else {
                                            LucideIcon::HardDrive
                                        })
                                        .size(px(14.))
                                        .flex_none()
                                        .text_color(c.foreground),
                                    )
                                    .child(div().min_w_0().truncate().child(revision_label))
                                    .child(
                                        lucide(LucideIcon::ChevronDown)
                                            .size(px(12.))
                                            .flex_none()
                                            .text_color(c.muted_foreground),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&filter).aria_label(if search_contents {
                                "Search file contents"
                            } else {
                                "Filter files"
                            })),
                    )
                    .child(
                        toggle_button(
                            "files.search-contents",
                            LucideIcon::TextSearch,
                            if search_contents {
                                "Filter file names"
                            } else {
                                "Search file contents"
                            },
                            search_contents,
                            &theme,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.set_search_contents(!search_contents, window, cx);
                            },
                        )),
                    )
                    .when_some(found, |this, (lines, truncated, files)| {
                        this.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(format!(
                                    "{}{lines} in {files} {}",
                                    if truncated { "First " } else { "" },
                                    if files == 1 { "file" } else { "files" }
                                )),
                        )
                    })
                    .when(
                        self.status == ListStatus::Ready && !search_contents,
                        |this| {
                            this.child(
                                div()
                                    .id("files.count")
                                    .flex_none()
                                    .text_xs()
                                    .text_color(c.muted_foreground)
                                    .tooltip(|_, cx| text_tooltip("Files Git tracks".into(), cx))
                                    .child(format!(
                                        "{count} {}",
                                        if count == 1 { "file" } else { "files" }
                                    )),
                            )
                        },
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_hits_by_file_and_finds_matches() {
        let hit = |path: &str, line| bonsai_git::GrepHit {
            path: PathBuf::from(path),
            line,
            text: String::new(),
        };
        let results = GrepResults {
            hits: vec![hit("a.rs", 1), hit("a.rs", 9), hit("b.rs", 3)],
            truncated: false,
        };
        assert_eq!(
            result_rows(&results),
            [
                ResultRow::File { first: 0, count: 2 },
                ResultRow::Hit(0),
                ResultRow::Hit(1),
                ResultRow::File { first: 2, count: 1 },
                ResultRow::Hit(2),
            ]
        );
        assert_eq!(match_ranges("Grind, grind", "grind"), [0..5, 7..12]);
        assert_eq!(match_ranges("Grind, grind", "Grind"), vec![(0..5)]);
    }

    #[test]
    fn builds_folders_before_files_and_filters_paths() {
        let paths: Vec<PathBuf> = ["README.md", "src/ui/tree.rs", "src/main.rs", "docs/a.md"]
            .map(PathBuf::from)
            .to_vec();
        let nodes = file_tree(&paths, "");
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_ref()).collect();
        assert_eq!(labels, ["docs", "src", "README.md"]);
        assert_eq!(nodes[1].id.as_ref(), "src/");
        let crate::ui::tree::TreeChildren::Loaded(src) = &nodes[1].children else {
            panic!("src is a folder");
        };
        assert_eq!(src[0].id.as_ref(), "src/ui/");
        assert_eq!(src[1].id.as_ref(), "src/main.rs");

        let filtered = file_tree(&paths, "TREE");
        assert_eq!(filtered.len(), 1);
        let mut folders = Vec::new();
        folder_ids(&filtered, &mut folders);
        assert_eq!(
            folders,
            ["src/", "src/ui/"].map(SharedString::from).to_vec()
        );
    }

    #[test]
    fn picks_a_grammar_and_parses_code_but_not_plain_text() {
        assert_eq!(language(Path::new("src/main.rs")), "rust");
        assert_eq!(language(Path::new("web/App.TSX")), "tsx");
        assert_eq!(language(Path::new("Makefile")), "make");
        assert_eq!(language(Path::new("LICENSE")), "text");
        let lines: Vec<SharedString> = ["fn main() {", "}"].map(SharedString::from).to_vec();
        let syntax = parse_syntax(Path::new("main.rs"), &lines).unwrap();
        assert_eq!(syntax.starts, [0, 12]);
        assert!(parse_syntax(Path::new("notes.txt"), &lines).is_none());
    }

    #[test]
    fn reads_text_binary_images_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "one\n\ttwo\n").unwrap();
        std::fs::write(root.join("b.bin"), [1, 0, 2]).unwrap();
        std::fs::write(root.join("c.png"), [0x89, b'P', b'N', b'G']).unwrap();
        match read_content(&root.join("a.txt")) {
            Content::Text { lines, bytes } => {
                assert_eq!(lines.as_slice(), ["one", "    two"]);
                assert_eq!(bytes, 9);
            }
            _ => panic!("text"),
        }
        assert!(matches!(
            read_content(&root.join("b.bin")),
            Content::Binary { bytes: 3 }
        ));
        assert!(matches!(
            read_content(&root.join("c.png")),
            Content::Image { .. }
        ));
        assert!(matches!(read_content(&root.join("gone")), Content::Missing));
        assert_eq!(byte_size(512), "512 bytes");
        assert_eq!(byte_size(1536), "1.5 KB");
    }
}
