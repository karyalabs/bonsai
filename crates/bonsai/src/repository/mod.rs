//! The app-side state for one open repository (ADR 5).

mod activity;
mod watcher;

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use std::path::PathBuf;

use bonsai_git::{
    CancelToken, CloneProgress, CommandRecord, Progress, RefState, Repository, ResetMode, Snapshot,
    Undo, UndoStep, plan_undo,
};
use gpui_kit::{AppContext as _, Context, EventEmitter, SharedString, Task};

pub use activity::Activity;
use activity::ActivitySink;

/// How long the watcher waits for file events to settle before refreshing.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(200);

/// How many operations Undo can go back.
const UNDO_LIMIT: usize = 50;

type OperationFn = Box<dyn FnOnce(&Repository) -> bonsai_git::Result<()> + Send>;

struct PendingOperation {
    name: SharedString,
    run: OperationFn,
    network: Option<NetworkRun>,
    /// A background run, whose failure the workspace doesn't announce.
    quiet: bool,
    hints: UndoHints,
    history: UndoRole,
}

/// What an operation tells undo beyond the refs it changes.
#[derive(Clone, Debug, Default)]
pub struct UndoHints {
    /// Files to save before it runs, so a discard can be undone.
    pub backup: Vec<PathBuf>,
    /// The mode, when it's a reset, so undo restores the index the same way.
    pub reset: Option<ResetMode>,
}

/// The refs before and after an operation, and the files saved before it.
type Recorded = ((RefState, RefState), Option<Vec<bonsai_git::FileBackup>>);

/// How an operation relates to the undo history.
enum UndoRole {
    /// An ordinary operation: its undo is recorded.
    Record,
    /// Undoing one: its reverse becomes the redo.
    Undo(Undo, Arc<Mutex<Option<Undo>>>),
    /// Redoing one: its reverse becomes the undo again.
    Redo(Undo, Arc<Mutex<Option<Undo>>>),
}

/// What a finished operation means for undo, so the workspace can offer it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoNote {
    None,
    /// It can be undone.
    Undoable,
    /// It was an undo, and can be redone.
    Redoable,
}

/// Progress and cancellation for an operation that talks to a remote.
#[derive(Clone)]
struct NetworkRun {
    progress: Arc<Mutex<Option<CloneProgress>>>,
    cancel: CancelToken,
}

/// Emitted by [`RepositoryModel`].
/// The name of the fetch [`RepositoryModel::fetch_in_background`] runs.
pub const BACKGROUND_FETCH: &str = "Fetch in the background";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepositoryEvent {
    SnapshotChanged,
    OperationFinished {
        name: SharedString,
        /// The error message when the operation failed.
        error: Option<SharedString>,
        /// Whether the app started it on its own, such as a background
        /// fetch, so a failure shouldn't interrupt the person.
        quiet: bool,
        undo: UndoNote,
    },
}

/// One open repository: its latest snapshot, its operation queue and its
/// activity log. Views observe it and keep only view state.
pub struct RepositoryModel {
    repository: Repository,
    snapshot: Option<Snapshot>,
    snapshot_error: Option<SharedString>,
    activity: Activity,
    sink: Arc<ActivitySink>,
    queue: VecDeque<PendingOperation>,
    running: Option<SharedString>,
    refresh_generation: u64,
    refresh_task: Option<Task<()>>,
    operation_task: Option<Task<()>>,
    network: Option<NetworkRun>,
    progress_task: Option<Task<()>>,
    undo: Vec<Undo>,
    redo: Vec<Undo>,
    _watcher: Option<watcher::Watcher>,
}

impl EventEmitter<RepositoryEvent> for RepositoryModel {}

impl RepositoryModel {
    /// Opens the model, reads the first snapshot and, when `watch` is set,
    /// refreshes on file changes.
    pub fn new(repository: Repository, watch: bool, cx: &mut Context<Self>) -> Self {
        let sink = Arc::new(ActivitySink::default());
        let git = repository.git().clone().with_log(sink.clone());
        let repository = repository.with_git(git);
        let watcher = watch
            .then(|| watcher::Watcher::start(&repository, WATCH_DEBOUNCE, cx))
            .flatten();
        let mut this = Self {
            repository,
            snapshot: None,
            snapshot_error: None,
            activity: Activity::default(),
            sink,
            queue: VecDeque::new(),
            running: None,
            refresh_generation: 0,
            refresh_task: None,
            operation_task: None,
            network: None,
            progress_task: None,
            undo: Vec::new(),
            redo: Vec::new(),
            _watcher: watcher,
        };
        this.refresh(cx);
        this
    }

    #[must_use]
    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    /// The latest snapshot; `None` until the first read finishes.
    #[must_use]
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    /// Why the latest snapshot read failed, if it did.
    #[must_use]
    pub fn snapshot_error(&self) -> Option<&SharedString> {
        self.snapshot_error.as_ref()
    }

    #[must_use]
    pub fn activity(&self) -> &Activity {
        &self.activity
    }

    /// The operation running now, if any.
    #[must_use]
    pub fn running(&self) -> Option<&SharedString> {
        self.running.as_ref()
    }

    /// The latest progress of the running network operation.
    #[must_use]
    pub fn progress(&self) -> Option<CloneProgress> {
        self.network
            .as_ref()
            .and_then(|n| n.progress.lock().ok().and_then(|p| p.clone()))
    }

    /// Whether the running operation can be cancelled.
    #[must_use]
    pub fn can_cancel(&self) -> bool {
        self.network.is_some()
    }

    /// Cancels the running network operation.
    pub fn cancel_running(&mut self) {
        if let Some(network) = &self.network {
            network.cancel.cancel();
        }
    }

    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Reads a fresh snapshot in the background. A result that a newer
    /// refresh or an operation has overtaken is dropped.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // An operation refreshes when it finishes; reading mid-operation would
        // show a half-done state.
        if self.running.is_some() {
            return;
        }
        self.refresh_generation += 1;
        let generation = self.refresh_generation;
        let repository = self.repository.clone();
        let read = cx.background_spawn(async move { repository.snapshot() });
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let result = read.await;
            let _ = this.update(cx, |this, cx| {
                if this.refresh_generation == generation && this.running.is_none() {
                    this.apply_snapshot(result, cx);
                }
            });
        }));
    }

    /// Queues a mutation. Operations run one at a time, in order, each ending
    /// with a fresh snapshot.
    pub fn run(
        &mut self,
        name: impl Into<SharedString>,
        operation: impl FnOnce(&Repository) -> bonsai_git::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.queue.push_back(PendingOperation {
            name: name.into(),
            run: Box::new(operation),
            network: None,
            quiet: false,
            hints: UndoHints::default(),
            history: UndoRole::Record,
        });
        self.pump(cx);
    }

    /// Like [`Self::run`], with hints that let undo reverse it exactly, such
    /// as the files a discard throws away.
    pub fn run_with(
        &mut self,
        name: impl Into<SharedString>,
        hints: UndoHints,
        operation: impl FnOnce(&Repository) -> bonsai_git::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.queue.push_back(PendingOperation {
            name: name.into(),
            run: Box::new(operation),
            network: None,
            quiet: false,
            hints,
            history: UndoRole::Record,
        });
        self.pump(cx);
    }

    /// The operation Undo would reverse, newest first.
    #[must_use]
    pub fn undo_name(&self) -> Option<&str> {
        self.undo.last().map(|undo| undo.name.as_str())
    }

    /// The operation Redo would repeat.
    #[must_use]
    pub fn redo_name(&self) -> Option<&str> {
        self.redo.last().map(|undo| undo.name.as_str())
    }

    /// Undoes the newest undoable operation. Returns false when there is none.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(undo) = self.undo.pop() else {
            return false;
        };
        self.queue_history(format!("Undo {}", undo.name), UndoRole::Undo, undo, cx);
        true
    }

    /// Repeats the newest undone operation. Returns false when there is none.
    pub fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(redo) = self.redo.pop() else {
            return false;
        };
        self.queue_history(format!("Redo {}", redo.name), UndoRole::Redo, redo, cx);
        true
    }

    fn queue_history(
        &mut self,
        name: String,
        role: fn(Undo, Arc<Mutex<Option<Undo>>>) -> UndoRole,
        undo: Undo,
        cx: &mut Context<Self>,
    ) {
        let slot = Arc::new(Mutex::new(None));
        let result = slot.clone();
        let steps = undo.clone();
        self.queue.push_back(PendingOperation {
            name: name.into(),
            run: Box::new(move |repository| {
                let reverse = repository.apply_undo(&steps)?;
                if let Ok(mut slot) = result.lock() {
                    *slot = reverse;
                }
                Ok(())
            }),
            network: None,
            quiet: false,
            hints: UndoHints::default(),
            history: role(undo, slot),
        });
        self.pump(cx);
    }

    /// Queues a fetch, pull, push or other remote operation, which reports
    /// progress and can be cancelled with [`Self::cancel_running`].
    pub fn run_network(
        &mut self,
        name: impl Into<SharedString>,
        operation: impl FnOnce(&Repository, Progress, CancelToken) -> bonsai_git::Result<()>
        + Send
        + 'static,
        cx: &mut Context<Self>,
    ) {
        self.queue_network(name.into(), operation, false, cx);
    }

    fn queue_network(
        &mut self,
        name: SharedString,
        operation: impl FnOnce(&Repository, Progress, CancelToken) -> bonsai_git::Result<()>
        + Send
        + 'static,
        quiet: bool,
        cx: &mut Context<Self>,
    ) {
        let network = NetworkRun {
            progress: Arc::new(Mutex::new(None)),
            cancel: CancelToken::new(),
        };
        let slot = network.progress.clone();
        let progress: Progress = Arc::new(move |update| {
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(update);
            }
        });
        let cancel = network.cancel.clone();
        self.queue.push_back(PendingOperation {
            name,
            run: Box::new(move |repository| operation(repository, progress, cancel)),
            network: Some(network),
            quiet,
            hints: UndoHints::default(),
            history: UndoRole::Record,
        });
        self.pump(cx);
    }

    /// Fetches every remote in the background when nothing else is running
    /// or queued, so it never delays what the person asked for. Returns
    /// whether the fetch was queued.
    pub fn fetch_in_background(&mut self, cx: &mut Context<Self>) -> bool {
        let has_remotes = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.remotes.is_empty());
        if self.running.is_some() || !self.queue.is_empty() || !has_remotes {
            return false;
        }
        self.queue_network(
            BACKGROUND_FETCH.into(),
            |repository, progress, cancel| {
                repository.fetch(None, true, Some(progress), Some(cancel))
            },
            true,
            cx,
        );
        true
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        let Some(operation) = self.queue.pop_front() else {
            return;
        };
        // Any refresh in flight is now stale.
        self.refresh_generation += 1;
        self.refresh_task = None;
        self.running = Some(operation.name.clone());
        self.network = operation.network;
        if self.network.is_some() {
            // Redraw progress while the operation runs.
            self.progress_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(150))
                        .await;
                    let running = this
                        .update(cx, |this, cx| {
                            cx.notify();
                            this.network.is_some()
                        })
                        .unwrap_or(false);
                    if !running {
                        break;
                    }
                }
            }));
        }
        cx.notify();

        let repository = self.repository.clone();
        let record = matches!(operation.history, UndoRole::Record) && !operation.quiet;
        let UndoHints { backup, reset } = operation.hints;
        let run = operation.run;
        let work = cx.background_spawn(async move {
            let before = if record {
                repository.ref_state().ok()
            } else {
                None
            };
            let files = if backup.is_empty() {
                None
            } else {
                match repository.back_up_files(&backup) {
                    Ok(files) => Some(files),
                    // Without a backup the discard would be final.
                    Err(error) => return (Err(error), repository.snapshot(), None),
                }
            };
            let result = run(&repository);
            let after = before.as_ref().and_then(|_| repository.ref_state().ok());
            let states = before.zip(after).map(|states| (states, files));
            (result, repository.snapshot(), states)
        });
        let name = operation.name;
        let quiet = operation.quiet;
        let history = operation.history;
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let (result, snapshot, states) = work.await;
            let _ = this.update(cx, |this, cx| {
                this.running = None;
                this.network = None;
                this.progress_task = None;
                let changes_before = this.snapshot.as_ref().map(|s| s.changes.clone());
                this.apply_snapshot(snapshot, cx);
                let changes_after = this.snapshot.as_ref().map(|s| &s.changes);
                let worktree_changed = changes_before.as_ref() != changes_after;
                let undo = if result.is_ok() {
                    this.note_history(&name, history, states, worktree_changed, reset)
                } else {
                    this.restore_history(history);
                    UndoNote::None
                };
                cx.emit(RepositoryEvent::OperationFinished {
                    name,
                    error: result.err().map(|e| e.to_string().into()),
                    quiet,
                    undo,
                });
                this.pump(cx);
            });
        }));
    }

    /// Records what a successful operation means for undo and redo.
    fn note_history(
        &mut self,
        name: &str,
        history: UndoRole,
        states: Option<Recorded>,
        worktree_changed: bool,
        reset: Option<ResetMode>,
    ) -> UndoNote {
        match history {
            UndoRole::Record => {
                let Some(((before, after), files)) = states else {
                    return UndoNote::None;
                };
                // Refs first: a reset back refuses over changed files, which
                // are written back once it's done.
                let mut steps = plan_undo(&before, &after, worktree_changed, reset);
                if let Some(files) = files.filter(|files| !files.is_empty()) {
                    steps.push(UndoStep::RestoreFiles { files });
                }
                if steps.is_empty() {
                    return UndoNote::None;
                }
                self.redo.clear();
                self.push_undo(Undo {
                    name: name.to_owned(),
                    steps,
                });
                UndoNote::Undoable
            }
            UndoRole::Undo(_, slot) => match slot.lock().ok().and_then(|mut slot| slot.take()) {
                Some(redo) => {
                    self.redo.push(redo);
                    UndoNote::Redoable
                }
                None => UndoNote::None,
            },
            UndoRole::Redo(_, slot) => match slot.lock().ok().and_then(|mut slot| slot.take()) {
                Some(undo) => {
                    self.push_undo(undo);
                    UndoNote::Undoable
                }
                None => UndoNote::None,
            },
        }
    }

    /// Puts an undo or redo that failed back, so it can be tried again.
    fn restore_history(&mut self, history: UndoRole) {
        match history {
            UndoRole::Record => {}
            UndoRole::Undo(undo, _) => self.undo.push(undo),
            UndoRole::Redo(redo, _) => self.redo.push(redo),
        }
    }

    fn push_undo(&mut self, undo: Undo) {
        self.undo.push(undo);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    fn apply_snapshot(&mut self, result: bonsai_git::Result<Snapshot>, cx: &mut Context<Self>) {
        self.activity.extend(self.sink.drain());
        match result {
            Ok(snapshot) => {
                self.snapshot_error = None;
                if self.snapshot.as_ref() != Some(&snapshot) {
                    self.snapshot = Some(snapshot);
                    cx.emit(RepositoryEvent::SnapshotChanged);
                }
            }
            Err(error) => self.snapshot_error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    /// Records commands run outside an operation, such as reads for a view.
    pub fn record(&mut self, record: CommandRecord, cx: &mut Context<Self>) {
        self.activity.push(record);
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
