//! Files: every tracked file as a folder tree, a file's contents, and the
//! actions on a file (Blame, its History, Purge from History, Reveal).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use bonsai_git::Oid;
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, Image, ImageFormat,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled, StyledImage as _, Subscription,
    UniformListScrollHandle, WeakEntity, Window, div, img, prelude::FluentBuilder as _, px,
    uniform_list,
};

use crate::{
    blame_view::{BlameEvent, BlameView},
    file_icon::file_icon,
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        context_menu::context_menu,
        input::{Input, InputEvent, InputState},
        menu::{MenuItem, MenuState},
        theme::UiTheme,
        tooltip::text_tooltip,
        tree::{Tree, TreeEvent, TreeNode, TreeRow, TreeRowKind, TreeState},
    },
};

/// Files larger than this show a note instead of their text.
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// A text row's height.
const ROW: f32 = 20.;

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

pub struct FilesView {
    model: Entity<RepositoryModel>,
    status: ListStatus,
    paths: Arc<Vec<PathBuf>>,
    tree: TreeState,
    filter: Option<(Entity<InputState>, Subscription)>,
    filter_text: String,
    /// The file the viewer shows.
    selected: Option<PathBuf>,
    content: Option<Content>,
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
    let data = match std::fs::read(absolute) {
        Ok(data) => data,
        Err(error) => return Content::Failed(error.to_string().into()),
    };
    if let Some(format) = image_format(absolute) {
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

impl FilesView {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            if matches!(event, RepositoryEvent::SnapshotChanged) && this.status != ListStatus::Idle
            {
                this.load(cx);
                this.reload_content(cx);
            }
        });
        let tree = TreeState::new(Vec::new(), []).expect("an empty tree has no duplicates");
        Self {
            model,
            status: ListStatus::Idle,
            paths: Arc::default(),
            tree,
            filter: None,
            filter_text: String::new(),
            selected: None,
            content: None,
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
        let task = cx.background_spawn(async move { repository.tracked_files() });
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

    fn ensure_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((input, _)) = &self.filter {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                this.set_filter(&text, cx);
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
        self.blame = None;
        self.scroll = UniformListScrollHandle::new();
        self.content = None;
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
        let absolute = self.model.read(cx).repository().root().join(path);
        let task = cx.background_spawn(async move { read_content(&absolute) });
        cx.spawn(async move |this, cx| {
            let content = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.content = Some(content);
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
        let view = cx.new(|cx| BlameView::new(&model, path, None, cx).close_label("Show File"));
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
        vec![
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
            MenuItem::new("files.reveal", "Reveal in Finder")
                .icon(LucideIcon::FolderOpen)
                .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
            MenuItem::new("files.open", "Open in Default App")
                .icon(LucideIcon::ExternalLink)
                .on_click(move |_, _, cx| cx.open_with_system(&absolute)),
            MenuItem::new("files.purge", "Purge from History…")
                .icon(LucideIcon::Eraser)
                .on_click(emit(&this, FilesEvent::Purge, &path)),
        ]
    }

    fn rebuild_menu(&mut self, cx: &mut Context<Self>) {
        let items = self.actions(cx);
        self.menu.update(cx, |menu, cx| menu.set_items(items, cx));
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
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_none()
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
                    }),
            )
            .when_some(facts, |this, facts| {
                this.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(facts),
                )
            })
            .child(div().flex_1())
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
            .child(
                icon_button(
                    "files.header.reveal",
                    LucideIcon::FolderOpen,
                    "Reveal in Finder",
                    theme,
                )
                .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
            )
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
        uniform_list("files.text", count, move |range, _, _| {
            range
                .map(|index| {
                    div()
                        .h(px(ROW))
                        .flex()
                        .items_center()
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
                                .child(lines[index].clone()),
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
    let style = theme.clone();
    Button::new(id)
        .aria_label(tip)
        .style_with(move |button| {
            crate::ui::button::style_button(
                button,
                false,
                ButtonVariant::Ghost,
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
            ListStatus::Ready => {
                context_menu(&self.menu, "File actions", self.render_tree(&theme, cx))
                    .size_full()
                    .into_any_element()
            }
        };
        let count = self.paths.len();
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
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&filter).aria_label("Filter files")),
                    )
                    .when(self.status == ListStatus::Ready, |this| {
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
                    }),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
