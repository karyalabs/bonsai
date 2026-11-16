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
mod integrate;
mod log;
mod patch;
mod path;
mod purge;
mod redact;
mod remote;
mod repository;
mod runner;
mod snapshot;
mod stash;
mod sync;
mod version;
mod worktree;

pub use branch::{CheckoutBlocker, is_valid_ref_name};
pub use clone::{CloneProgress, clone_name};
pub use commit::Signing;
pub use diff::{
    DiffOptions, DiffOutput, DiffTarget, FileVersion, LineCount, LineCounts, Whitespace,
    parse_numstat,
};
pub use error::{Error, Result};
pub use integrate::{Integration, MergeMode, OperationAction, ResetMode, Side};
pub use log::{CommitDetails, CommitInfo, LogFilter, LogReader, LogScope, SignatureStatus};
pub use patch::{ApplyMode, LinePosition, LineSide, hunk_changes, partial_patch};
pub use purge::{PurgeBlocker, PurgeReport, PurgeScan, PurgeStep, PurgeTarget, RefChange};
pub use remote::RemoteFailure;
pub use repository::Repository;
pub use runner::{
    ASKPASS_MODE_ENV, CancelToken, CommandLog, CommandRecord, Git, GitCommand, Output, OutputStream,
};
pub use snapshot::{
    Branch, Change, ChangeKind, ConflictKind, EntryStatus, Head, InProgress, Oid, Remote,
    RemoteBranch, Snapshot, Stash, Tag, Tracking,
};
pub use stash::{StashContents, stash_diff};
pub use sync::{Progress, PullMode, PushRequest, RewrittenBranch, SyncFailure};
pub use version::GitVersion;
pub use worktree::IgnoreRule;
