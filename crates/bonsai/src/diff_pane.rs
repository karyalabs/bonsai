//! The detail pane's diff: loads a patch in the background and shows it with
//! GPUI Kit's diff, unified or side by side.

use std::{path::PathBuf, sync::Arc};

use bonsai_git::{
    ApplyMode, CommitDetails, DiffOptions, DiffOutput, DiffTarget, FileVersion, LfsPointer,
    LinePosition, LineSide, Oid, SignatureStatus, SubmoduleCommit, SubmoduleMove, Whitespace,
    format_bytes, hunk_changes, parse_lfs_patch, parse_submodule_patch, partial_patch,
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, EventEmitter, Image, ImageFormat,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled, StyledImage as _, Subscription, Window,
    component::diff::{DiffFile, DiffLineRange, DiffMode, DiffSide, DiffState},
    div, img,
    prelude::FluentBuilder as _,
    px,
};

use crate::{
    blame_view::{BlameEvent, BlameView},
    confirm::{self, Confirm},
    file_icon::file_icon,
    repository::{RepositoryEvent, RepositoryModel, UndoHints},
    ui::{
        button::{Button, ButtonSize, ButtonVariant, style_button},
        dialog::{DialogHandle, dialog},
        diff::{DiffAction, DiffIntent, DiffView},
        menu::{Menu, MenuItem, MenuState},
        theme::UiTheme,
        tooltip::text_tooltip,
    },
    workspace::title_bar::ghost_trigger,
};

/// A Git LFS file's pointer on each side of a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LfsDiff {
    pub path: SharedString,
    pub before: Option<LfsPointer>,
    pub after: Option<LfsPointer>,
}

impl LfsDiff {
    fn read(files: &[DiffFile], patch: &str) -> Option<Self> {
        let [file] = files else { return None };
        let (before, after) = parse_lfs_patch(patch)?;
        Some(Self {
            path: file.path().as_ref().to_owned().into(),
            before,
            after,
        })
    }
}

/// The two sides of a changed image, for a preview instead of a patch.
#[derive(Clone, Default)]
pub struct ImagePreview {
    pub before: Option<Arc<Image>>,
    pub after: Option<Arc<Image>>,
}

/// The image format for `path`'s extension, if GPUI can draw it.
fn image_format(path: &std::path::Path) -> Option<ImageFormat> {
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

/// Why a commit's lines could not be reverted: the file no longer has them
/// as that commit left them.
const REVERT_CONFLICT: &str =
    "These lines have changed since that commit, so they can't be reverted on their own.";

/// The one file `target` shows, when it shows one.
fn single_path(target: &DiffTarget) -> Option<PathBuf> {
    match target {
        DiffTarget::Unstaged(paths) | DiffTarget::Staged(paths) if paths.len() == 1 => {
            Some(paths[0].clone())
        }
        DiffTarget::Untracked(path) => Some(path.clone()),
        _ => None,
    }
}

/// Where the old and new sides of `target`'s files come from.
fn versions(target: &DiffTarget) -> (Option<FileVersion>, FileVersion) {
    match target {
        DiffTarget::Unstaged(_) => (Some(FileVersion::Index), FileVersion::WorkTree),
        DiffTarget::Staged(_) => (Some(FileVersion::Commit("HEAD".into())), FileVersion::Index),
        DiffTarget::Untracked(_) => (None, FileVersion::WorkTree),
        DiffTarget::Commit(oid) => (
            Some(FileVersion::Commit(format!("{}^", oid.as_str()))),
            FileVersion::Commit(oid.as_str().to_owned()),
        ),
        DiffTarget::Range { from, to } => (
            Some(FileVersion::Commit(from.as_str().to_owned())),
            FileVersion::Commit(to.as_str().to_owned()),
        ),
    }
}

/// The whole-hunk ranges of every file in `patch`, for the gutter buttons.
fn hunk_ranges(patch: &str, files: &[DiffFile]) -> Vec<DiffLineRange> {
    let side = |side: LineSide| match side {
        LineSide::Old => DiffSide::Original,
        LineSide::New => DiffSide::Modified,
    };
    files
        .iter()
        .flat_map(|file| {
            let file_path = file.path().clone();
            hunk_changes(patch, &file_path)
                .into_iter()
                .map(move |(first, last)| {
                    DiffLineRange::new(file_path.clone(), side(first.side), first.line, last.line)
                        .with_end_side(side(last.side))
                })
        })
        .collect()
}

/// A small icon button for the diff toolbar; `active` marks a toggle that is on.
fn tool_button(
    id: &'static str,
    icon: LucideIcon,
    tip: impl Into<SharedString>,
    active: bool,
    theme: &UiTheme,
) -> Button {
    let tip: SharedString = tip.into();
    let style_theme = theme.clone();
    Button::new(id)
        .aria_label(tip.clone())
        .style_with(move |button| {
            let tip = tip.clone();
            style_button(
                button,
                false,
                if active {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                },
                ButtonSize::IconSm,
                &style_theme,
            )
            .tooltip(move |_, cx| text_tooltip(tip.clone(), cx))
        })
        .child(
            lucide(icon)
                .size(px(15.))
                .text_color(theme.colors.foreground),
        )
}

/// What the pane shows instead of a diff, or that a diff is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneStatus {
    Nothing,
    Loading,
    Ready,
    /// Nothing differs, for example after `-w` hid every change.
    Empty,
    TooLarge {
        bytes: usize,
    },
    Failed(SharedString),
}

pub struct DiffPane {
    model: Entity<RepositoryModel>,
    state: Entity<DiffState>,
    target: Option<DiffTarget>,
    title: SharedString,
    status: PaneStatus,
    whitespace: Whitespace,
    /// The patch on screen, for building partial patches from a selection.
    patch: Option<String>,
    /// The shown commit's message, people and signature.
    details: Option<CommitDetails>,
    discard: DialogHandle,
    /// The partial patch the open Discard Lines dialog will reverse.
    pending_discard: Option<String>,
    /// Why the last line action could not run.
    error: Option<SharedString>,
    /// The first and last changed line of every hunk on screen.
    hunks: Vec<DiffLineRange>,
    /// The shown image's two sides, instead of a binary patch.
    preview: Option<ImagePreview>,
    /// The commits a submodule moved across, instead of its one-line patch.
    submodule: Option<SubmoduleMove>,
    /// A Git LFS file's pointers, instead of their text.
    lfs: Option<LfsDiff>,
    /// Wraps long lines instead of scrolling sideways.
    wrap: bool,
    /// Shows every line of the file, not just the changes and their context.
    full_file: bool,
    more: Entity<MenuState>,
    /// Blame for one file, shown instead of the diff.
    blame: Option<(Entity<BlameView>, Subscription)>,
    /// Ignores results for a target that has since changed.
    generation: u64,
    _events: Subscription,
}

impl DiffPane {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| DiffState::new(Vec::new(), cx));
        let events = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            // Work tree diffs follow the files; commit diffs never change.
            if matches!(event, RepositoryEvent::SnapshotChanged)
                && matches!(
                    this.target,
                    Some(
                        DiffTarget::Unstaged(_) | DiffTarget::Staged(_) | DiffTarget::Untracked(_)
                    )
                )
            {
                this.load(false, cx);
            }
        });
        Self {
            model,
            state,
            target: None,
            title: SharedString::default(),
            status: PaneStatus::Nothing,
            whitespace: Whitespace::Show,
            patch: None,
            details: None,
            discard: DialogHandle::new(false),
            pending_discard: None,
            error: None,
            hunks: Vec::new(),
            preview: None,
            submodule: None,
            lfs: None,
            wrap: false,
            full_file: false,
            more: cx.new(|cx| MenuState::new(Vec::new(), cx).width(px(240.))),
            blame: None,
            generation: 0,
            _events: events,
        }
    }

    #[must_use]
    pub fn status(&self) -> &PaneStatus {
        &self.status
    }

    #[must_use]
    pub fn state(&self) -> &Entity<DiffState> {
        &self.state
    }

    #[must_use]
    pub fn target(&self) -> Option<&DiffTarget> {
        self.target.as_ref()
    }

    /// Shows `target`, or the empty state for `None`.
    pub fn show(
        &mut self,
        target: Option<DiffTarget>,
        title: SharedString,
        cx: &mut Context<Self>,
    ) {
        if self.target == target {
            return;
        }
        self.blame = None;
        self.target = target;
        self.title = title;
        self.rebuild_more(cx);
        self.load(false, cx);
    }

    fn load(&mut self, any_size: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let Some(target) = self.target.clone() else {
            self.status = PaneStatus::Nothing;
            self.state
                .update(cx, |state, cx| state.set_files(Vec::new(), cx));
            cx.notify();
            return;
        };
        if self.status != PaneStatus::Ready {
            self.status = PaneStatus::Loading;
        }
        let repository = self.model.read(cx).repository().clone();
        let options = DiffOptions {
            whitespace: self.whitespace,
            max_bytes: if any_size {
                None
            } else {
                DiffOptions::default().max_bytes
            },
            ..DiffOptions::default()
        };
        let details_target = target.clone();
        let details_repository = repository.clone();
        let details = cx.background_spawn(async move {
            match details_target {
                DiffTarget::Commit(oid) => details_repository.commit_details(&oid).ok(),
                _ => None,
            }
        });
        let preview_repository = repository.clone();
        let preview_target = target.clone();
        let task = cx.background_spawn(async move {
            repository
                .diff(&target, options)
                .map_err(|e| e.to_string())
                .and_then(|output| match output {
                    DiffOutput::Patch(patch) => DiffFile::parse(&patch)
                        .map(|files| Ok((files, patch)))
                        .map_err(|e| format!("Bonsai could not read this diff: {e:?}")),
                    DiffOutput::TooLarge { bytes } => Ok(Err(bytes)),
                })
                .map(|result| {
                    result.map(|(files, patch)| {
                        let preview =
                            Self::load_preview(&preview_repository, &preview_target, &files);
                        let submodule = Self::load_submodule(&preview_repository, &files, &patch);
                        let lfs = LfsDiff::read(&files, &patch);
                        (files, patch, (preview, submodule, lfs))
                    })
                })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let details = details.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.details = details;
                this.patch = None;
                this.hunks.clear();
                this.preview = None;
                this.submodule = None;
                this.lfs = None;
                this.status = match result {
                    Ok(Ok((files, _, _))) if files.is_empty() => {
                        this.state
                            .update(cx, |state, cx| state.set_files(Vec::new(), cx));
                        PaneStatus::Empty
                    }
                    Ok(Ok((files, patch, (preview, submodule, lfs)))) => {
                        this.hunks = hunk_ranges(&patch, &files);
                        this.patch = Some(patch);
                        this.preview = preview;
                        this.submodule = submodule;
                        this.lfs = lfs;
                        this.state
                            .update(cx, |state, cx| state.set_files(files, cx));
                        PaneStatus::Ready
                    }
                    Ok(Err(bytes)) => PaneStatus::TooLarge { bytes },
                    Err(error) => PaneStatus::Failed(error.into()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_whitespace(&mut self, whitespace: Whitespace, cx: &mut Context<Self>) {
        if self.whitespace != whitespace {
            self.whitespace = whitespace;
            self.load(false, cx);
        }
    }

    /// Both sides of a changed image, when `files` is one binary image.
    fn load_preview(
        repository: &bonsai_git::Repository,
        target: &DiffTarget,
        files: &[DiffFile],
    ) -> Option<ImagePreview> {
        let [file] = files else { return None };
        if !file.is_binary() {
            return None;
        }
        let path = PathBuf::from(file.path().as_ref());
        let format = image_format(&path)?;
        let original = file
            .original_path()
            .map_or_else(|| path.clone(), |p| PathBuf::from(p.as_ref()));
        let (before, after) = versions(target);
        let read = |path: &std::path::Path, version: &FileVersion| {
            repository
                .file_version(path, version)
                .ok()
                .flatten()
                .map(|bytes| Arc::new(Image::from_bytes(format, bytes)))
        };
        Some(ImagePreview {
            before: before.and_then(|version| read(&original, &version)),
            after: read(&path, &after),
        })
    }

    #[must_use]
    pub fn preview(&self) -> Option<&ImagePreview> {
        self.preview.as_ref()
    }

    /// The submodule move on screen, when the diff is one submodule.
    #[must_use]
    pub fn submodule(&self) -> Option<&SubmoduleMove> {
        self.submodule.as_ref()
    }

    /// Whether something other than a patch is on screen.
    fn has_preview(&self) -> bool {
        self.preview.is_some() || self.submodule.is_some() || self.lfs.is_some()
    }

    /// The Git LFS pointers on screen, when the diff is one LFS file.
    #[must_use]
    pub fn lfs(&self) -> Option<&LfsDiff> {
        self.lfs.as_ref()
    }

    /// The commits a submodule moved across, when `files` is one submodule.
    fn load_submodule(
        repository: &bonsai_git::Repository,
        files: &[DiffFile],
        patch: &str,
    ) -> Option<SubmoduleMove> {
        let [file] = files else { return None };
        let (from, to, dirty) = parse_submodule_patch(patch)?;
        let submodule = PathBuf::from(file.path().as_ref());
        Some(repository.submodule_move(&submodule, from, to, dirty))
    }

    #[must_use]
    pub fn hunks(&self) -> &[DiffLineRange] {
        &self.hunks
    }

    fn set_full_file(&mut self, full: bool, cx: &mut Context<Self>) {
        self.full_file = full;
        self.state.update(cx, |state, cx| {
            if full {
                state.expand_unchanged(cx);
            } else {
                state.collapse_unchanged(cx);
            }
        });
        cx.notify();
    }

    /// Shows who last changed each line of `path`, as `rev` has it or as
    /// the work tree has it, in place of the diff.
    pub fn open_blame(&mut self, path: PathBuf, rev: Option<Oid>, cx: &mut Context<Self>) {
        let model = self.model.clone();
        let view = cx.new(|cx| BlameView::new(&model, path, rev, cx));
        let events = cx.subscribe(&view, |this, _, event: &BlameEvent, cx| match event {
            BlameEvent::Close => this.close_blame(cx),
            BlameEvent::ShowCommit(oid) => cx.emit(DiffPaneEvent::ShowInHistory(oid.clone())),
        });
        self.blame = Some((view, events));
        cx.notify();
    }

    pub fn close_blame(&mut self, cx: &mut Context<Self>) {
        self.blame = None;
        cx.notify();
    }

    #[must_use]
    pub fn blame_view(&self) -> Option<&Entity<BlameView>> {
        self.blame.as_ref().map(|(view, _)| view)
    }

    /// The More menu: copy, reveal and open the shown file.
    fn rebuild_more(&mut self, cx: &mut Context<Self>) {
        let mut items = Vec::new();
        if let Some(path) = self.target.as_ref().and_then(single_path) {
            let absolute = self.model.read(cx).repository().root().join(&path);
            let copy = path.to_string_lossy().into_owned();
            let reveal = absolute.clone();
            items.push(
                MenuItem::new("diff.more.copy-path", "Copy Path")
                    .icon(LucideIcon::Copy)
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    }),
            );
            items.push(
                MenuItem::new("diff.more.reveal", "Reveal in Finder")
                    .icon(LucideIcon::FolderOpen)
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
            );
            items.push(
                MenuItem::new("diff.more.open", "Open in Default App")
                    .icon(LucideIcon::ExternalLink)
                    .on_click(move |_, _, cx| cx.open_with_system(&absolute)),
            );
        }
        self.more.update(cx, |menu, cx| menu.set_items(items, cx));
    }

    /// The line actions the shown diff offers: none for commits, untracked
    /// files, or while whitespace is hidden (the patch would not apply).
    fn line_actions(&self) -> Vec<DiffAction> {
        if self.whitespace != Whitespace::Show {
            return Vec::new();
        }
        let mut actions = match self.target {
            Some(DiffTarget::Unstaged(_)) => vec![
                DiffAction::new("stage", "Stage Lines").icon(LucideIcon::Plus),
                DiffAction::new("discard", "Discard Lines…")
                    .icon(LucideIcon::Undo2)
                    .destructive(true),
            ],
            Some(DiffTarget::Staged(_)) => {
                vec![DiffAction::new("unstage", "Unstage Lines").icon(LucideIcon::Minus)]
            }
            Some(DiffTarget::Commit(_)) => {
                vec![DiffAction::new("revert", "Revert Lines").icon(LucideIcon::Undo2)]
            }
            _ => Vec::new(),
        };
        if self.target.is_some() {
            actions.push(
                DiffAction::new("history", "Find Where This Changed").icon(LucideIcon::Clock),
            );
        }
        actions
    }

    /// The first added or removed line in a selection, trimmed: what to
    /// search history for.
    fn selected_code(&self, range: &DiffLineRange) -> Option<String> {
        let partial = self.partial(range, ApplyMode::Stage)?;
        partial
            .lines()
            .filter(|line| !line.starts_with("+++") && !line.starts_with("---"))
            .find_map(|line| line.strip_prefix('+').or_else(|| line.strip_prefix('-')))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    }

    /// The gutter button beside each hunk: Stage for unstaged changes,
    /// Unstage for staged ones, Revert for a commit's.
    fn hunk_action(&self) -> Option<DiffAction> {
        if self.whitespace != Whitespace::Show {
            return None;
        }
        match self.target {
            Some(DiffTarget::Unstaged(_)) => {
                Some(DiffAction::new("stage", "Stage Hunk").icon(LucideIcon::Plus))
            }
            Some(DiffTarget::Staged(_)) => {
                Some(DiffAction::new("unstage", "Unstage Hunk").icon(LucideIcon::Minus))
            }
            Some(DiffTarget::Commit(_)) => {
                Some(DiffAction::new("revert", "Revert Hunk").icon(LucideIcon::Undo2))
            }
            _ => None,
        }
    }

    /// Stages, unstages or (after confirming) discards the selected lines.
    pub fn act(&mut self, intent: &DiffIntent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = &intent.range else { return };
        if intent.action.as_ref() == "history" {
            match self.selected_code(range) {
                Some(code) => {
                    self.error = None;
                    cx.emit(DiffPaneEvent::SearchHistory(code));
                }
                None => {
                    self.error = Some("Select an added or removed line to search for.".into());
                }
            }
            cx.notify();
            return;
        }
        let mode = match intent.action.as_ref() {
            "stage" => ApplyMode::Stage,
            "unstage" => ApplyMode::Unstage,
            "discard" | "revert" => ApplyMode::Discard,
            _ => return,
        };
        let Some(partial) = self.partial(range, mode) else {
            self.error = Some("Select at least one added or removed line.".into());
            cx.notify();
            return;
        };
        self.error = None;
        if intent.action.as_ref() == "revert" {
            self.revert(partial, PathBuf::from(range.path().as_ref()), cx);
            return;
        }
        if mode == ApplyMode::Discard {
            self.pending_discard = Some(partial);
            self.discard.open(window, cx);
            cx.notify();
            return;
        }
        self.apply(partial, mode, cx);
    }

    fn partial(&self, range: &DiffLineRange, mode: ApplyMode) -> Option<String> {
        let side = |side: DiffSide| match side {
            DiffSide::Original => LineSide::Old,
            DiffSide::Modified => LineSide::New,
        };
        partial_patch(
            self.patch.as_deref()?,
            range.path(),
            LinePosition::new(side(range.side()), range.start()),
            LinePosition::new(side(range.end_side()), range.end()),
            mode,
        )
    }

    fn apply(&mut self, partial: String, mode: ApplyMode, cx: &mut Context<Self>) {
        let name = match mode {
            ApplyMode::Stage => "Stage lines",
            ApplyMode::Unstage => "Unstage lines",
            ApplyMode::Discard => "Discard lines",
        };
        // Discarding lines rewrites the file; save it so undo can restore it.
        let backup = match (&self.target, mode) {
            (Some(DiffTarget::Unstaged(paths)), ApplyMode::Discard) => paths.clone(),
            _ => Vec::new(),
        };
        let hints = UndoHints {
            backup,
            reset: None,
        };
        self.model.update(cx, |model, cx| {
            model.run_with(
                name,
                hints,
                move |repository| repository.apply_patch(&partial, mode),
                cx,
            );
        });
    }

    /// Takes a commit's selected changes back out of the work tree, as a
    /// revert of just those lines would. The file is saved first, so ⌘Z
    /// puts it back.
    fn revert(&mut self, partial: String, path: PathBuf, cx: &mut Context<Self>) {
        let hints = UndoHints {
            backup: vec![path],
            reset: None,
        };
        self.model.update(cx, |model, cx| {
            model.run_with(
                "Revert lines",
                hints,
                move |repository| {
                    repository
                        .apply_patch(&partial, ApplyMode::Discard)
                        .map_err(|error| match error {
                            bonsai_git::Error::Failed { .. } => {
                                bonsai_git::Error::Refused(REVERT_CONFLICT.into())
                            }
                            other => other,
                        })
                },
                cx,
            );
        });
    }

    fn render_discard(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let popup = confirm::popup(
            "diff.discard",
            "Discard these lines?",
            "The selected changes go back to the staged or committed version. You can undo this with ⌘Z.",
            cx,
        )
        .child(
            Confirm::new(
                "diff.discard",
                "Discard",
                |this: &mut Self, window, cx| {
                    this.pending_discard = None;
                    this.discard.close(window, cx);
                },
                |this, window, cx| {
                    this.discard.close(window, cx);
                    if let Some(partial) = this.pending_discard.take() {
                        this.apply(partial, ApplyMode::Discard, cx);
                    }
                },
            )
            .footer(cx),
        );
        let this = cx.entity().downgrade();
        dialog("diff.discard", &self.discard, popup, window, cx).on_cancel(move |_, _, cx| {
            let _ = this.update(cx, |pane, cx| {
                pane.pending_discard = None;
                cx.notify();
            });
            true
        })
    }

    #[must_use]
    pub fn details(&self) -> Option<&CommitDetails> {
        self.details.as_ref()
    }

    fn render_details(
        &self,
        details: &CommitDetails,
        theme: &UiTheme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let c = &theme.colors;
        let when = |time: i64| {
            u64::try_from(time)
                .ok()
                .map(|s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s))
                .map(crate::activity_panel::ago)
                .unwrap_or_default()
        };
        let (signature, signature_color, signature_icon) = match details.signature {
            SignatureStatus::Unsigned => (None, c.muted_foreground, LucideIcon::Shield),
            SignatureStatus::Good => (Some("Verified"), c.success, LucideIcon::ShieldCheck),
            SignatureStatus::GoodUntrusted => {
                (Some("Untrusted key"), c.warning, LucideIcon::ShieldAlert)
            }
            SignatureStatus::Bad => (Some("Bad signature"), c.destructive, LucideIcon::ShieldX),
            SignatureStatus::Unknown => {
                (Some("Unverified"), c.muted_foreground, LucideIcon::Shield)
            }
        };
        let (summary, body) = details
            .message
            .split_once('\n')
            .map_or((details.message.as_str(), ""), |(s, b)| (s, b.trim()));
        let short = |oid: &bonsai_git::Oid| oid.as_str().chars().take(7).collect::<String>();
        let dark = theme.mode == crate::ui::theme::ThemeMode::Dark;
        let full_hash = details.oid.as_str().to_owned();
        let mut byline = format!("authored {}", when(details.author_time));
        if details.committer_email != details.author_email {
            byline.push_str(" · committed by ");
            byline.push_str(&details.committer_name);
        }
        let pill = |text: String| {
            div()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(c.muted)
                .font_family(theme.fonts.mono.clone())
                .text_xs()
                .child(text)
        };
        let files = self.state.read(cx).files().to_vec();
        let file_rows = files.iter().enumerate().map(|(ix, file)| {
            let path = file.path().clone();
            let (folder, name) = path
                .rsplit_once('/')
                .map_or((None, path.to_string()), |(f, n)| {
                    (Some(f.to_owned()), n.to_owned())
                });
            let (letter, color) = match file.status() {
                gpui_kit::component::diff::DiffFileStatus::Added => ('A', c.success),
                gpui_kit::component::diff::DiffFileStatus::Deleted => ('D', c.destructive),
                gpui_kit::component::diff::DiffFileStatus::Renamed
                | gpui_kit::component::diff::DiffFileStatus::Copied => ('R', c.info),
                _ => ('M', c.warning),
            };
            let target = path.clone();
            let blame_path = PathBuf::from(path.as_ref());
            let blame_rev = match &self.target {
                Some(DiffTarget::Commit(oid)) => Some(oid.clone()),
                _ => None,
            };
            let deleted = matches!(
                file.status(),
                gpui_kit::component::diff::DiffFileStatus::Deleted
            );
            div()
                .id(("diff.commit.file", ix))
                .group("commit-file")
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .h(px(26.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|this| this.bg(c.muted))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state
                        .update(cx, |state, cx| state.scroll_to_file(&target, cx));
                }))
                .child(
                    lucide(file_icon(std::path::Path::new(path.as_ref())))
                        .size(px(14.))
                        .flex_shrink_0()
                        .text_color(c.muted_foreground),
                )
                .child(div().flex_shrink_0().text_sm().child(name))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .when_some(folder, gpui_kit::ParentElement::child),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .text_xs()
                        .font_family(theme.fonts.mono.clone())
                        .when(file.additions() > 0, |this| {
                            this.child(
                                div()
                                    .text_color(c.success)
                                    .child(format!("+{}", file.additions())),
                            )
                        })
                        .when(file.deletions() > 0, |this| {
                            this.child(
                                div()
                                    .text_color(c.destructive)
                                    .child(format!("−{}", file.deletions())),
                            )
                        }),
                )
                .when(!deleted, |this| {
                    this.child(
                        div()
                            .id(("diff.commit.file.blame", ix))
                            .flex_shrink_0()
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .invisible()
                            .group_hover("commit-file", gpui_kit::Styled::visible)
                            .hover(|this| this.bg(c.accent))
                            .tooltip(|_, cx| text_tooltip("Blame at This Commit".into(), cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_blame(blame_path.clone(), blame_rev.clone(), cx);
                            }))
                            .child(
                                lucide(LucideIcon::ScrollText)
                                    .size(px(12.))
                                    .text_color(c.muted_foreground),
                            ),
                    )
                })
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(18.))
                        .h(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .bg(color.opacity(0.15))
                        .text_color(color)
                        .font_family(theme.fonts.mono.clone())
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::BOLD)
                        .child(letter.to_string()),
                )
        });
        let (added, removed) = files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.additions(), r + f.deletions()));
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(px(14.))
            .py(px(12.))
            .border_b_1()
            .border_color(c.border)
            .child(
                div()
                    .text_base()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(summary.to_owned()),
            )
            .when(!body.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child(body.to_owned()),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(crate::avatar::avatar(
                        &details.author_name,
                        &details.author_email,
                        px(28.),
                        dark,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.))
                                    .text_sm()
                                    .child(
                                        div()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .child(details.author_name.clone()),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(c.muted_foreground)
                                            .child(details.author_email.clone()),
                                    ),
                            )
                            .child(div().text_xs().text_color(c.muted_foreground).child(byline)),
                    )
                    .child(
                        div()
                            .id("diff.commit.hash")
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(c.border)
                            .cursor_pointer()
                            .hover(|this| this.bg(c.muted))
                            .tooltip(|_, cx| text_tooltip("Copy Full Hash".into(), cx))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(full_hash.clone()));
                            })
                            .child(
                                div()
                                    .font_family(theme.fonts.mono.clone())
                                    .text_xs()
                                    .child(short(&details.oid)),
                            )
                            .child(
                                lucide(LucideIcon::Copy)
                                    .size(px(12.))
                                    .text_color(c.muted_foreground),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(match details.parents.len() {
                        0 => "Root commit".to_owned(),
                        1 => "Parent".to_owned(),
                        _ => "Parents".to_owned(),
                    })
                    .children(details.parents.iter().map(|p| pill(short(p))))
                    .when_some(signature, |this, signature| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .ml(px(6.))
                                .text_color(signature_color)
                                .child(
                                    lucide(signature_icon)
                                        .size(px(12.))
                                        .text_color(signature_color),
                                )
                                .child(signature),
                        )
                    }),
            )
            .when(files.len() > 1, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(c.muted_foreground)
                                .child(format!("{} FILES CHANGED", files.len()))
                                .child(div().text_color(c.success).child(format!("+{added}")))
                                .child(
                                    div().text_color(c.destructive).child(format!("−{removed}")),
                                ),
                        )
                        .child(
                            div()
                                .id("diff.commit.files")
                                .max_h(px(182.))
                                .overflow_y_scroll()
                                .children(file_rows),
                        ),
                )
            })
    }

    /// The file (or commit) on the left; navigation, view options and the
    /// More menu on the right.
    fn render_toolbar(&self, theme: &UiTheme, cx: &mut Context<Self>) -> gpui_kit::Div {
        let c = &theme.colors;
        let state = self.state.read(cx);
        let mode = state.mode();
        let (added, removed) = state
            .files()
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.additions(), r + f.deletions()));
        let path = self.target.as_ref().and_then(single_path);
        let title = if let Some(path) = &path {
            {
                let text = path.to_string_lossy();
                let (folder, name) = text
                    .rsplit_once('/')
                    .map_or((None, text.as_ref()), |(f, n)| (Some(f.to_owned()), n));
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        lucide(file_icon(path))
                            .size(px(15.))
                            .flex_shrink_0()
                            .text_color(c.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(name.to_owned()),
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
            }
        } else {
            {
                // A commit's card shows its message, so the bar names the commit.
                let text: SharedString = match &self.target {
                    Some(DiffTarget::Commit(oid)) => format!(
                        "Commit {}",
                        oid.as_str().chars().take(7).collect::<String>()
                    )
                    .into(),
                    _ => self.title.clone(),
                };
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(matches!(self.target, Some(DiffTarget::Commit(_))), |this| {
                        this.child(
                            lucide(LucideIcon::GitCommitHorizontal)
                                .size(px(15.))
                                .flex_shrink_0()
                                .text_color(c.muted_foreground),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(text),
                    )
            }
        };
        let line_stats = (self.status == PaneStatus::Ready
            && !self.has_preview()
            && path.is_some())
        .then(|| {
            div()
                .flex()
                .flex_shrink_0()
                .gap(px(6.))
                .text_xs()
                .font_family(theme.fonts.mono.clone())
                .when(added > 0, |this| {
                    this.child(div().text_color(c.success).child(format!("+{added}")))
                })
                .when(removed > 0, |this| {
                    this.child(div().text_color(c.destructive).child(format!("−{removed}")))
                })
        });
        let diff_tools = self.status == PaneStatus::Ready && !self.has_preview();
        let separator = || div().w(px(1.)).h(px(16.)).mx(px(2.)).bg(c.border);
        let whitespace_hidden = self.whitespace != Whitespace::Show;
        let tools = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(2.))
            .when(diff_tools, |this| {
                this.child(
                    tool_button(
                        "diff.previous",
                        LucideIcon::ChevronUp,
                        "Previous Change",
                        false,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.update(cx, DiffState::previous_change);
                    })),
                )
                .child(
                    tool_button(
                        "diff.next",
                        LucideIcon::ChevronDown,
                        "Next Change",
                        false,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.update(cx, DiffState::next_change);
                    })),
                )
                .child(separator())
                .child(
                    tool_button(
                        "diff.full-file",
                        if self.full_file {
                            LucideIcon::FoldVertical
                        } else {
                            LucideIcon::UnfoldVertical
                        },
                        if self.full_file {
                            "Show Changes Only"
                        } else {
                            "Show Entire File"
                        },
                        self.full_file,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_full_file(!this.full_file, cx);
                    })),
                )
                .child(
                    tool_button(
                        "diff.wrap",
                        LucideIcon::TextWrap,
                        if self.wrap {
                            "Don't Wrap Lines"
                        } else {
                            "Wrap Lines"
                        },
                        self.wrap,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.wrap = !this.wrap;
                        cx.notify();
                    })),
                )
            })
            .when(diff_tools || self.status == PaneStatus::Empty, |this| {
                this.child(
                    tool_button(
                        "diff.whitespace",
                        LucideIcon::Space,
                        if whitespace_hidden {
                            "Show Whitespace Changes"
                        } else {
                            "Hide Whitespace Changes"
                        },
                        whitespace_hidden,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let next = match this.whitespace {
                            Whitespace::Show => Whitespace::IgnoreAll,
                            Whitespace::IgnoreAll | Whitespace::IgnoreChanges => Whitespace::Show,
                        };
                        this.set_whitespace(next, cx);
                    })),
                )
            })
            .when(diff_tools, |this| {
                let unified = mode != DiffMode::Split;
                this.child(separator())
                    .child(
                        tool_button(
                            "diff.layout.unified",
                            LucideIcon::Rows2,
                            "Unified",
                            unified,
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state
                                .update(cx, |state, cx| state.set_mode(DiffMode::Unified, cx));
                            cx.notify();
                        })),
                    )
                    .child(
                        tool_button(
                            "diff.layout.split",
                            LucideIcon::Columns2,
                            "Side by Side",
                            !unified,
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state
                                .update(cx, |state, cx| state.set_mode(DiffMode::Split, cx));
                            cx.notify();
                        })),
                    )
            })
            .when_some(
                path.clone()
                    .filter(|_| !matches!(self.target, Some(DiffTarget::Untracked(_)))),
                |this, path| {
                    this.child(separator()).child(
                        tool_button("diff.blame", LucideIcon::ScrollText, "Blame", false, theme)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_blame(path.clone(), None, cx);
                            })),
                    )
                },
            )
            .when(path.is_some(), |this| {
                this.child(
                    Menu::new(&self.more, "More")
                        .trigger_style_with(ghost_trigger(theme, ButtonSize::IconSm))
                        .trigger(
                            lucide(LucideIcon::Ellipsis)
                                .size(px(15.))
                                .text_color(c.foreground),
                        ),
                )
            });
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(40.))
            .px(px(12.))
            .border_b_1()
            .border_color(c.border)
            .child(title)
            .children(line_stats)
            .child(div().flex_1())
            .child(tools)
    }

    /// A Git LFS file: its size and content hash on each side.
    fn render_lfs(lfs: &LfsDiff, theme: &UiTheme) -> gpui_kit::Div {
        let c = &theme.colors;
        let mono = theme.fonts.mono.clone();
        let side = |label: &'static str, pointer: Option<&LfsPointer>, color| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(10.))
                .child(div().w(px(3.)).h(px(28.)).rounded(px(2.)).bg(color))
                .child(
                    div()
                        .w(px(56.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(label),
                )
                .child(match pointer {
                    Some(pointer) => div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(format_bytes(pointer.size)),
                        )
                        .child(
                            div()
                                .font_family(mono.clone())
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(format!("sha256 {}", pointer.short())),
                        ),
                    None => div()
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child("Not stored with LFS"),
                })
        };
        let headline = match (&lfs.before, &lfs.after) {
            (Some(before), Some(after)) if after.size >= before.size => {
                format!("{} larger", format_bytes(after.size - before.size))
            }
            (Some(before), Some(after)) => {
                format!("{} smaller", format_bytes(before.size - after.size))
            }
            (None, Some(_)) => "Added to Git LFS".to_owned(),
            (Some(_), None) => "No longer in Git LFS".to_owned(),
            (None, None) => String::new(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        lucide(LucideIcon::HardDrive)
                            .size(px(20.))
                            .text_color(c.foreground),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(format!("{} is stored with Git LFS", lfs.path)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(c.muted_foreground)
                                    .child(headline),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(c.border)
                    .child(side("Before", lfs.before.as_ref(), c.destructive))
                    .child(
                        side("After", lfs.after.as_ref(), c.success)
                            .border_t_1()
                            .border_color(c.border),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(
                        "Git keeps a small pointer in each commit; the contents live on the LFS server and download when you check them out.",
                    ),
            )
    }

    /// A submodule's new commit: where it moved and the commits on the way.
    fn render_submodule(
        moved: &SubmoduleMove,
        theme: &UiTheme,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let c = &theme.colors;
        let mono = theme.fonts.mono.clone();
        let short = |oid: &Option<Oid>| {
            oid.as_ref()
                .map(|o| o.short().to_string())
                .unwrap_or_default()
        };
        let headline = match (&moved.from, &moved.to) {
            (Some(_), Some(_)) => {
                format!("Moved from {} to {}", short(&moved.from), short(&moved.to))
            }
            (None, Some(_)) => format!("Added at {}", short(&moved.to)),
            (Some(_), None) => format!("Removed, was at {}", short(&moved.from)),
            (None, None) => "Changed".to_owned(),
        };
        let commit_row = |commit: &SubmoduleCommit, color| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(6.))
                .border_b_1()
                .border_color(c.border)
                .text_sm()
                .child(div().w(px(3.)).h(px(16.)).rounded(px(2.)).bg(color))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(commit.oid.short().to_string()),
                )
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
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(commit.author.clone()),
                )
        };
        let section = |title: String, commits: &[SubmoduleCommit], color| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(c.muted_foreground)
                        .child(title),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(c.border)
                        .overflow_hidden()
                        .children(commits.iter().map(|commit| commit_row(commit, color))),
                )
        };
        let count = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        div()
            .id("diff.submodule")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        lucide(LucideIcon::Package)
                            .size(px(20.))
                            .text_color(c.foreground),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(format!("Submodule {}", moved.path.display())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(c.muted_foreground)
                                    .child(headline),
                            ),
                    ),
            )
            .when(moved.dirty, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(c.warning)
                        .child("The submodule also has uncommitted changes of its own."),
                )
            })
            .when(!moved.readable, |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child("Clone the submodule to see the commits in between."),
                )
            })
            .when(!moved.added.is_empty(), |this| {
                this.child(section(
                    count(moved.added.len(), "new commit"),
                    &moved.added,
                    c.success,
                ))
            })
            .when(!moved.removed.is_empty(), |this| {
                this.child(section(
                    format!(
                        "{} no longer included",
                        count(moved.removed.len(), "commit")
                    ),
                    &moved.removed,
                    c.destructive,
                ))
            })
    }

    /// A changed image: the old and new versions side by side.
    fn render_preview(preview: &ImagePreview, theme: &UiTheme) -> gpui_kit::Div {
        let c = &theme.colors;
        let byte_size = |image: &Arc<Image>| {
            let bytes = image.bytes.len();
            // Tenths of a unit, in integers: "4.5 KB".
            let tenths = |unit: usize| {
                let tenths = (bytes * 10).div_ceil(unit);
                format!("{}.{}", tenths / 10, tenths % 10)
            };
            if bytes < 1024 {
                format!("{bytes} B")
            } else if bytes < 1024 * 1024 {
                format!("{} KB", tenths(1024))
            } else {
                format!("{} MB", tenths(1024 * 1024))
            }
        };
        let side = |label: &'static str, image: Option<&Arc<Image>>, color| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(div().text_color(color).child(label))
                        .when_some(image, |this, image| {
                            this.child(div().text_color(c.muted_foreground).child(byte_size(image)))
                        }),
                )
                .child(
                    div()
                        .h(px(320.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(12.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(c.border)
                        .bg(c.muted.opacity(0.4))
                        .map(|this| match image {
                            Some(image) => this.child(
                                img(image.clone())
                                    .max_w_full()
                                    .max_h_full()
                                    .object_fit(ObjectFit::Contain),
                            ),
                            None => this.child(
                                div()
                                    .text_sm()
                                    .text_color(c.muted_foreground)
                                    .child("No image"),
                            ),
                        }),
                )
        };
        div()
            .size_full()
            .flex()
            .gap(px(16.))
            .p(px(16.))
            .when(preview.before.is_some(), |this| {
                this.child(side("Before", preview.before.as_ref(), c.destructive))
            })
            .child(side(
                if preview.before.is_some() {
                    "After"
                } else {
                    "Added"
                },
                preview.after.as_ref(),
                c.success,
            ))
    }

    fn render_message(
        icon: LucideIcon,
        title: &str,
        detail: Option<String>,
        theme: &UiTheme,
    ) -> gpui_kit::Div {
        let c = &theme.colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .p(px(24.))
            .child(lucide(icon).size(px(20.)).text_color(c.muted_foreground))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(title.to_owned()),
            )
            .when_some(detail, |this, detail| {
                this.child(
                    div()
                        .max_w(px(420.))
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child(detail),
                )
            })
    }
}

pub enum DiffPaneEvent {
    /// Blame asked to show this commit in History.
    ShowInHistory(Oid),
    /// Find the commits that added or removed this code.
    SearchHistory(String),
}

impl EventEmitter<DiffPaneEvent> for DiffPane {}

impl Render for DiffPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let body = match &self.status {
            PaneStatus::Nothing => Self::render_message(
                LucideIcon::FileDiff,
                "Nothing selected",
                Some("Select a change or a commit to see its diff.".into()),
                &theme,
            )
            .into_any_element(),
            PaneStatus::Loading => div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Loading the diff…")
                .into_any_element(),
            PaneStatus::Empty => Self::render_message(
                LucideIcon::Equal,
                "No differences",
                (self.whitespace != Whitespace::Show)
                    .then(|| "Whitespace changes are hidden.".to_owned()),
                &theme,
            )
            .into_any_element(),
            PaneStatus::TooLarge { bytes } => Self::render_message(
                LucideIcon::TriangleAlert,
                "This diff is large",
                Some(format!(
                    "It is {} MB. Showing it may take a moment.",
                    bytes.div_ceil(1_048_576)
                )),
                &theme,
            )
            .child(
                Button::new("diff.load-anyway")
                    .variant(ButtonVariant::Outline)
                    .label("Show Anyway")
                    .on_click(cx.listener(|this, _, _, cx| this.load(true, cx))),
            )
            .into_any_element(),
            PaneStatus::Failed(error) => Self::render_message(
                LucideIcon::TriangleAlert,
                "Bonsai could not load this diff",
                Some(error.to_string()),
                &theme,
            )
            .into_any_element(),
            PaneStatus::Ready if self.submodule.is_some() => {
                Self::render_submodule(self.submodule.as_ref().expect("checked above"), &theme)
                    .into_any_element()
            }
            PaneStatus::Ready if self.lfs.is_some() => {
                Self::render_lfs(self.lfs.as_ref().expect("checked above"), &theme)
                    .into_any_element()
            }
            PaneStatus::Ready if self.preview.is_some() => {
                Self::render_preview(self.preview.as_ref().expect("checked above"), &theme)
                    .into_any_element()
            }
            PaneStatus::Ready => {
                let actions = self.line_actions();
                let view = cx.entity().downgrade();
                let single = self.target.as_ref().and_then(single_path).is_some();
                DiffView::new("diff.view", &self.state)
                    .read_only(actions.is_empty())
                    .selection_actions(actions)
                    .when_some(self.hunk_action(), |view, action| {
                        view.hunk_action(action, self.hunks.clone())
                    })
                    .header_visible(!single)
                    .soft_wrap(self.wrap)
                    .on_intent(move |intent, window, cx| {
                        let _ = view.update(cx, |pane, cx| pane.act(intent, window, cx));
                    })
                    .into_any_element()
            }
        };
        if let Some((blame, _)) = &self.blame {
            return div()
                .size_full()
                .child(blame.clone())
                .child(self.render_discard(window, cx));
        }
        let toolbar = self.render_toolbar(&theme, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .when(self.target.is_some(), |this| this.child(toolbar))
            .when_some(
                self.details
                    .as_ref()
                    .filter(|_| matches!(self.target, Some(DiffTarget::Commit(_)))),
                |this, details| this.child(self.render_details(details, &theme, cx)),
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
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_discard(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Git, Repository};
    use gpui_kit::TestAppContext;

    use super::*;

    #[test]
    fn reads_an_lfs_files_pointers_from_its_patch() {
        let patch = "diff --git a/cover.psd b/cover.psd\nindex 80d7d50..9a95d3b 100644\n--- a/cover.psd\n+++ b/cover.psd\n@@ -1,3 +1,3 @@\n version https://git-lfs.github.com/spec/v1\n-oid sha256:aaaa\n-size 2048\n+oid sha256:bbbb\n+size 4096\n";
        let files = DiffFile::parse(patch).unwrap();
        let lfs = LfsDiff::read(&files, patch).expect("an LFS file");
        assert_eq!(lfs.path.as_ref(), "cover.psd");
        assert_eq!(lfs.before.unwrap().size, 2048);
        assert_eq!(lfs.after.unwrap().size, 4096);
    }

    #[gpui_kit::test]
    fn loads_work_tree_diffs_and_follows_the_files(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let git = Git::locate(None).unwrap();
        let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
        std::fs::write(dir.path().join("notes.md"), "# Notes\n").unwrap();
        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let pane = cx.new(|cx| DiffPane::new(model.clone(), cx));
        cx.run_until_parked();

        pane.update(cx, |pane, cx| {
            pane.show(
                Some(DiffTarget::Untracked("notes.md".into())),
                "notes.md".into(),
                cx,
            );
        });
        cx.run_until_parked();
        pane.read_with(cx, |pane, cx| {
            assert_eq!(pane.status(), &PaneStatus::Ready);
            let state = pane.state().read(cx);
            assert_eq!(state.files().len(), 1);
            assert_eq!(state.files()[0].additions(), 1);
        });

        // Staging the file empties the untracked diff on the next snapshot.
        model.update(cx, |model, cx| {
            model.run("Stage", |r| r.stage(&["notes.md".into()]), cx);
        });
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| assert_eq!(pane.status(), &PaneStatus::Ready));

        pane.update(cx, |pane, cx| pane.show(None, SharedString::default(), cx));
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.status(), &PaneStatus::Nothing);
        });
    }

    #[gpui_kit::test]
    fn stages_and_discards_selected_lines(cx: &mut TestAppContext) {
        use crate::ui::diff::DiffIntent;
        use gpui_kit::component::diff::{DiffLineRange, DiffSide};

        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let git = Git::locate(None).unwrap();
        let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
        let run = |args: &[&str]| {
            git.run(&git.command(args.iter().copied()).cwd(dir.path()))
                .unwrap()
                .stdout_lossy()
        };
        run(&["config", "user.name", "Ada"]);
        run(&["config", "user.email", "ada@example.com"]);
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\n").unwrap();
        run(&["add", "."]);
        run(&["-c", "commit.gpgsign=false", "commit", "-qm", "base"]);
        std::fs::write(dir.path().join("f.txt"), "a\nX\nb\nY\nc\n").unwrap();

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (pane, cx) = cx.add_window_view(|_, cx| DiffPane::new(model.clone(), cx));
        cx.run_until_parked();
        pane.update(cx, |pane, cx| {
            pane.show(Some(DiffTarget::Unstaged(Vec::new())), "f.txt".into(), cx);
        });
        cx.run_until_parked();
        let intent = |action: &str, line: usize| DiffIntent {
            action: action.into(),
            path: "f.txt".into(),
            range: Some(DiffLineRange::new("f.txt", DiffSide::Modified, line, line)),
        };

        // Find Where This Changed asks History for the selected line's text.
        let searched = std::rc::Rc::new(std::cell::RefCell::new(None));
        let _events = cx.update({
            let searched = searched.clone();
            |_, cx| {
                cx.subscribe(&pane, move |_, event: &DiffPaneEvent, _| {
                    if let DiffPaneEvent::SearchHistory(code) = event {
                        *searched.borrow_mut() = Some(code.clone());
                    }
                })
            }
        });
        pane.update_in(cx, |pane, window, cx| {
            pane.act(&intent("history", 4), window, cx);
        });
        assert_eq!(searched.borrow().as_deref(), Some("Y"));

        pane.update_in(cx, |pane, window, cx| {
            assert_eq!(pane.line_actions().len(), 3);
            assert_eq!(pane.hunks().len(), 1, "X and Y share a hunk");
            assert!(pane.hunk_action().is_some());
            pane.act(&intent("stage", 2), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(run(&["show", ":f.txt"]), "a\nX\nb\nc\n");

        // Discarding asks first, then reverts only the chosen line.
        pane.update_in(cx, |pane, window, cx| {
            pane.act(&intent("discard", 4), window, cx);
            assert!(pane.pending_discard.is_some());
            assert!(pane.discard.is_open());
            let partial = pane.pending_discard.take().unwrap();
            pane.apply(partial, ApplyMode::Discard, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "a\nX\nb\nc\n"
        );

        // A staged diff offers Unstage Lines only.
        pane.update(cx, |pane, cx| {
            pane.show(Some(DiffTarget::Staged(Vec::new())), "f.txt".into(), cx);
        });
        cx.run_until_parked();
        pane.update_in(cx, |pane, window, cx| {
            assert_eq!(pane.line_actions().len(), 2);
            pane.act(&intent("unstage", 2), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(run(&["show", ":f.txt"]), "a\nb\nc\n");
    }

    #[gpui_kit::test]
    fn reverts_chosen_lines_of_a_commit(cx: &mut TestAppContext) {
        use crate::ui::diff::DiffIntent;
        use gpui_kit::component::diff::{DiffLineRange, DiffSide};

        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let git = Git::locate(None).unwrap();
        let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
        let run = |args: &[&str]| {
            git.run(&git.command(args.iter().copied()).cwd(dir.path()))
                .unwrap()
                .stdout_lossy()
        };
        run(&["config", "user.name", "Ada"]);
        run(&["config", "user.email", "ada@example.com"]);
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "a\nb\nc\n").unwrap();
        run(&["add", "."]);
        run(&["-c", "commit.gpgsign=false", "commit", "-qm", "base"]);
        std::fs::write(&file, "a\nX\nb\nY\nc\n").unwrap();
        run(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qam",
            "add X and Y",
        ]);
        let added = Oid::new(run(&["rev-parse", "HEAD"]).trim());
        // A later commit, so the revert lands on a file that moved on.
        std::fs::write(&file, "a\nX\nb\nY\nc\nd\n").unwrap();
        run(&["-c", "commit.gpgsign=false", "commit", "-qam", "add d"]);

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (pane, cx) = cx.add_window_view(|_, cx| DiffPane::new(model.clone(), cx));
        cx.run_until_parked();
        pane.update(cx, |pane, cx| {
            pane.show(Some(DiffTarget::Commit(added)), "add X and Y".into(), cx);
        });
        cx.run_until_parked();
        let intent = |line: usize| DiffIntent {
            action: "revert".into(),
            path: "f.txt".into(),
            range: Some(DiffLineRange::new("f.txt", DiffSide::Modified, line, line)),
        };
        pane.update_in(cx, |pane, window, cx| {
            assert_eq!(pane.line_actions().len(), 2, "Revert Lines and Find");
            assert!(pane.hunk_action().is_some());
            pane.act(&intent(4), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "a\nX\nb\nc\nd\n",
            "only Y goes, and the later d stays"
        );
        assert_eq!(
            run(&["show", ":f.txt"]),
            "a\nX\nb\nY\nc\nd\n",
            "the index is untouched"
        );

        // Y is gone now, so reverting it again explains why it can't.
        let error = std::rc::Rc::new(std::cell::RefCell::new(None));
        let _events = cx.update({
            let error = error.clone();
            |_, cx| {
                cx.subscribe(&model, move |_, event: &RepositoryEvent, _| {
                    if let RepositoryEvent::OperationFinished { error: Some(e), .. } = event {
                        *error.borrow_mut() = Some(e.to_string());
                    }
                })
            }
        });
        pane.update_in(cx, |pane, window, cx| pane.act(&intent(4), window, cx));
        cx.run_until_parked();
        assert_eq!(error.borrow().as_deref(), Some(REVERT_CONFLICT));
    }

    #[gpui_kit::test]
    fn blames_a_file_and_asks_history_for_its_commit(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let git = Git::locate(None).unwrap();
        let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
        let run = |args: &[&str]| {
            git.run(&git.command(args.iter().copied()).cwd(dir.path()))
                .unwrap()
                .stdout_lossy()
        };
        run(&["config", "user.name", "Ada"]);
        run(&["config", "user.email", "ada@example.com"]);
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\n").unwrap();
        run(&["add", "."]);
        run(&["-c", "commit.gpgsign=false", "commit", "-qm", "base"]);
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\nthree\n").unwrap();
        let head = run(&["rev-parse", "HEAD"]).trim().to_owned();

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (pane, cx) = cx.add_window_view(|_, cx| DiffPane::new(model.clone(), cx));
        cx.run_until_parked();
        pane.update(cx, |pane, cx| {
            pane.show(
                Some(DiffTarget::Unstaged(vec!["f.txt".into()])),
                "f.txt".into(),
                cx,
            );
            pane.open_blame("f.txt".into(), None, cx);
        });
        cx.run_until_parked();
        let view = pane.read_with(cx, |pane, _| pane.blame_view().cloned().unwrap());
        view.read_with(cx, |view, _| {
            assert_eq!(view.status(), &crate::blame_view::BlameStatus::Ready);
            assert_eq!(view.blame().lines.len(), 3);
            assert_eq!(view.blame().commit_of(0).unwrap().oid.as_str(), head);
            assert!(view.blame().commit_of(2).unwrap().is_uncommitted());
        });

        let shown = std::rc::Rc::new(std::cell::RefCell::new(None));
        let _events = cx.update({
            let shown = shown.clone();
            |_, cx| {
                cx.subscribe(&pane, move |_, event: &DiffPaneEvent, _| {
                    if let DiffPaneEvent::ShowInHistory(oid) = event {
                        *shown.borrow_mut() = Some(oid.clone());
                    }
                })
            }
        });
        view.update(cx, |_, cx| {
            cx.emit(BlameEvent::ShowCommit(Oid::new(head.clone())));
        });
        assert_eq!(
            shown.borrow().as_ref().map(Oid::as_str),
            Some(head.as_str())
        );

        // Close returns to the diff; a new target also leaves blame.
        view.update(cx, |_, cx| cx.emit(BlameEvent::Close));
        pane.read_with(cx, |pane, _| assert!(pane.blame_view().is_none()));
        pane.update(cx, |pane, cx| {
            pane.open_blame("f.txt".into(), Some(Oid::new(head)), cx);
        });
        pane.update(cx, |pane, cx| pane.show(None, SharedString::default(), cx));
        pane.read_with(cx, |pane, _| assert!(pane.blame_view().is_none()));
    }

    #[gpui_kit::test]
    fn previews_a_changed_image_and_stages_whole_hunks(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let git = Git::locate(None).unwrap();
        let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
        let run = |args: &[&str]| {
            git.run(&git.command(args.iter().copied()).cwd(dir.path()))
                .unwrap()
                .stdout_lossy()
        };
        run(&["config", "user.name", "Ada"]);
        run(&["config", "user.email", "ada@example.com"]);
        let lines: String = (1..=20).map(|n| n.to_string() + "\n").collect();
        std::fs::write(dir.path().join("f.txt"), &lines).unwrap();
        std::fs::write(dir.path().join("logo.png"), [0x89_u8, b'P', 0, 1]).unwrap();
        run(&["add", "."]);
        run(&["-c", "commit.gpgsign=false", "commit", "-qm", "base"]);
        std::fs::write(dir.path().join("logo.png"), [0x89_u8, b'P', 0, 2, 3]).unwrap();
        let edited = lines
            .replacen("2\n", "two\n", 1)
            .replace("19\n", "nineteen\n");
        std::fs::write(dir.path().join("f.txt"), &edited).unwrap();

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (pane, cx) = cx.add_window_view(|_, cx| DiffPane::new(model.clone(), cx));
        cx.run_until_parked();
        pane.update(cx, |pane, cx| {
            pane.show(
                Some(DiffTarget::Unstaged(vec!["logo.png".into()])),
                "logo.png".into(),
                cx,
            );
        });
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            let preview = pane.preview().expect("a changed image is previewed");
            assert_eq!(preview.before.as_ref().unwrap().bytes, [0x89, b'P', 0, 1]);
            assert_eq!(preview.after.as_ref().unwrap().bytes, [0x89, b'P', 0, 2, 3]);
        });

        pane.update(cx, |pane, cx| {
            pane.show(
                Some(DiffTarget::Unstaged(vec!["f.txt".into()])),
                "f.txt".into(),
                cx,
            );
        });
        cx.run_until_parked();
        let hunk = pane.read_with(cx, |pane, _| {
            assert!(pane.preview().is_none());
            assert_eq!(pane.hunks().len(), 2);
            pane.hunks()[1].clone()
        });
        pane.update_in(cx, |pane, window, cx| {
            let intent = DiffIntent {
                action: "stage".into(),
                path: "f.txt".into(),
                range: Some(hunk),
            };
            pane.act(&intent, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            run(&["show", ":f.txt"]),
            lines.replace("19\n", "nineteen\n")
        );
    }
}
