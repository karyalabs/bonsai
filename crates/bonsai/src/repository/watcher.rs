//! Refreshes a repository when its files change (`FSEvents` on macOS).

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use bonsai_git::Repository;
use futures::{StreamExt as _, channel::mpsc};
use gpui_kit::{Context, Task};
use notify::{RecursiveMode, Watcher as _};

use super::RepositoryModel;

pub(super) struct Watcher {
    _watcher: notify::RecommendedWatcher,
    _task: Task<()>,
}

impl Watcher {
    /// Starts watching; `None` when the platform watcher cannot start, in
    /// which case the app still refreshes after its own operations.
    pub(super) fn start(
        repository: &Repository,
        debounce: Duration,
        cx: &mut Context<RepositoryModel>,
    ) -> Option<Self> {
        let (sender, mut receiver) = mpsc::unbounded::<()>();
        let git_dir = repository.git_dir().to_path_buf();
        let filter_dir = git_dir.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if let Ok(event) = event
                    && event
                        .paths
                        .iter()
                        .any(|path| is_relevant(path, &filter_dir))
                {
                    let _ = sender.unbounded_send(());
                }
            })
            .ok()?;
        watcher
            .watch(repository.root(), RecursiveMode::Recursive)
            .ok()?;
        if !git_dir.starts_with(repository.root()) {
            // A linked worktree's git dir lives elsewhere.
            watcher.watch(&git_dir, RecursiveMode::Recursive).ok()?;
        }

        let task = cx.spawn(async move |this, cx| {
            while receiver.next().await.is_some() {
                cx.background_executor().timer(debounce).await;
                while receiver.try_recv().is_ok() {}
                if this.update(cx, RepositoryModel::refresh).is_err() {
                    break;
                }
            }
        });
        Some(Self {
            _watcher: watcher,
            _task: task,
        })
    }
}

/// Object writes and lock files never change what a snapshot shows by
/// themselves; the ref or index update that follows does.
fn is_relevant(path: &Path, git_dir: &Path) -> bool {
    if path.extension().is_some_and(|ext| ext == "lock") {
        return false;
    }
    if let Ok(inside) = path.strip_prefix(git_dir) {
        let first: PathBuf = inside.components().take(1).collect();
        return !matches!(first.to_str(), Some("objects" | "logs" | "lfs"));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::is_relevant;
    use std::path::Path;

    #[test]
    fn ignores_objects_logs_and_locks() {
        let git_dir = Path::new("/r/.git");
        assert!(is_relevant(Path::new("/r/src/main.rs"), git_dir));
        assert!(is_relevant(Path::new("/r/.git/index"), git_dir));
        assert!(is_relevant(Path::new("/r/.git/refs/heads/main"), git_dir));
        assert!(!is_relevant(Path::new("/r/.git/objects/ab/cdef"), git_dir));
        assert!(!is_relevant(Path::new("/r/.git/logs/HEAD"), git_dir));
        assert!(!is_relevant(Path::new("/r/.git/index.lock"), git_dir));
    }
}
