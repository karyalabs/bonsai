//! The app-side state for one open repository (ADR 5).

mod activity;
mod watcher;

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use bonsai_git::{CancelToken, CloneProgress, CommandRecord, Progress, Repository, Snapshot};
use gpui_kit::{AppContext as _, Context, EventEmitter, SharedString, Task};

pub use activity::Activity;
use activity::ActivitySink;

/// How long the watcher waits for file events to settle before refreshing.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(200);

type OperationFn = Box<dyn FnOnce(&Repository) -> bonsai_git::Result<()> + Send>;

struct PendingOperation {
    name: SharedString,
    run: OperationFn,
    network: Option<NetworkRun>,
}

/// Progress and cancellation for an operation that talks to a remote.
#[derive(Clone)]
struct NetworkRun {
    progress: Arc<Mutex<Option<CloneProgress>>>,
    cancel: CancelToken,
}

/// Emitted by [`RepositoryModel`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepositoryEvent {
    SnapshotChanged,
    OperationFinished {
        name: SharedString,
        /// The error message when the operation failed.
        error: Option<SharedString>,
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
            name: name.into(),
            run: Box::new(move |repository| operation(repository, progress, cancel)),
            network: Some(network),
        });
        self.pump(cx);
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
        let work = cx.background_spawn(async move {
            let result = (operation.run)(&repository);
            (result, repository.snapshot())
        });
        let name = operation.name;
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let (result, snapshot) = work.await;
            let _ = this.update(cx, |this, cx| {
                this.running = None;
                this.network = None;
                this.progress_task = None;
                this.apply_snapshot(snapshot, cx);
                cx.emit(RepositoryEvent::OperationFinished {
                    name,
                    error: result.err().map(|e| e.to_string().into()),
                });
                this.pump(cx);
            });
        }));
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
