//! Typed Git reads and operations for Bonsai.
//!
//! Every mutation and every network call runs the system `git` through
//! [`Git`], so hooks, signing, credential helpers and configuration behave as
//! they do in the user's terminal (ADR 2). Views never spawn processes; they go
//! through [`Repository`].

mod branch;
mod clone;
mod commit;
mod diff;
mod error;
mod log;
mod path;
mod purge;
mod redact;
mod remote;
mod repository;
mod runner;
mod snapshot;
mod sync;
mod version;
mod worktree;

pub use branch::{CheckoutBlocker, is_valid_ref_name};
pub use clone::{CloneProgress, clone_name};
pub use commit::Signing;
pub use diff::{DiffOptions, DiffOutput, DiffTarget, Whitespace};
pub use error::{Error, Result};
pub use log::{CommitInfo, LogScope};
pub use purge::{PurgeBlocker, PurgeReport, PurgeScan, PurgeStep, PurgeTarget, RefChange};
pub use remote::RemoteFailure;
pub use repository::Repository;
pub use runner::{CancelToken, CommandLog, CommandRecord, Git, GitCommand, Output};
pub use snapshot::{
    Branch, Change, ChangeKind, ConflictKind, EntryStatus, Head, InProgress, Oid, Remote,
    RemoteBranch, Snapshot, Stash, Tag, Tracking,
};
pub use sync::{Progress, PullMode, PushRequest, RewrittenBranch, SyncFailure};
pub use version::GitVersion;
pub use worktree::IgnoreRule;
