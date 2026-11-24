//! Typed Git reads and operations for Bonsai.
//!
//! Every mutation and every network call runs the system `git` through
//! [`Git`], so hooks, signing, credential helpers and configuration behave as
//! they do in the user's terminal (ADR 2). Views never spawn processes; they go
//! through [`Repository`].

mod bisect;
mod blame;
mod branch;
mod clone;
mod commit;
mod compare;
mod conflict;
mod diff;
mod error;
mod files;
mod integrate;
mod lfs;
mod log;
mod patch;
mod path;
mod purge;
mod rebase;
mod redact;
mod reflog;
mod remote;
mod repo_settings;
mod repository;
mod runner;
mod scaffold;
mod snapshot;
mod stash;
mod submodules;
mod sync;
mod undo;
mod version;
mod worktree;
mod worktrees;

pub use bisect::{BisectCommit, BisectState, BisectVerdict};
pub use blame::{Blame, BlameCommit, BlameLine, parse_porcelain};
pub use branch::{CheckoutBlocker, StaleBranch, StaleReason, is_valid_ref_name};
pub use clone::{CloneProgress, clone_name};
pub use commit::Signing;
pub use compare::Comparison;
pub use conflict::{Conflict, Resolution, Segment, parse_conflicts, resolve_conflicts, side_name};
pub use diff::{
    DiffOptions, DiffOutput, DiffTarget, FileVersion, LineCount, LineCounts, Whitespace,
    parse_numstat,
};
pub use error::{Error, Result};
pub use files::{GrepHit, GrepResults};
pub use integrate::{Integration, MergeMode, OperationAction, ResetMode, Side};
pub use lfs::{LfsPointer, lfs_pattern_for, lfs_patterns, parse_lfs_patch, parse_lfs_pointer};
pub use log::{CommitDetails, CommitInfo, LogFilter, LogReader, LogScope, SignatureStatus};
pub use patch::{ApplyMode, LinePosition, LineSide, hunk_changes, partial_patch};
pub use purge::{
    HistoryScan, PurgeBlocker, PurgeReport, PurgeScan, PurgeStep, PurgeTarget, RefChange,
    ScanFinding, ScanOptions, ScanReason, credentials_in, format_bytes, sensitive_name,
};
pub use rebase::{RebaseAction, RebaseStep, rebase_todo};
pub use reflog::{ReflogAction, ReflogEntry};
pub use remote::RemoteFailure;
pub use repo_settings::{ConfigValue, Hook, RepoSettings, RepoSettingsEdit, SigningChoice};
pub use repository::Repository;
pub use runner::{
    ASKPASS_MODE_ENV, CancelToken, CommandLog, CommandRecord, Git, GitCommand, Output, OutputStream,
};
pub use scaffold::{IgnoreTemplate, License, NewRepository, is_empty_or_missing};
pub use snapshot::{
    Branch, Change, ChangeKind, ConflictKind, EntryStatus, Head, InProgress, Oid, Remote,
    RemoteBranch, Snapshot, Stash, Tag, Tracking,
};
pub use stash::{StashContents, stash_diff};
pub use submodules::{
    Submodule, SubmoduleCommit, SubmoduleMove, SubmoduleState, parse_submodule_patch,
    parse_submodule_status,
};
pub use sync::{Progress, PullMode, PushRequest, RewrittenBranch, SyncFailure};
pub use undo::{FileBackup, HeadRef, RefState, Undo, UndoReset, UndoStep, plan_undo};
pub use version::GitVersion;
pub use worktree::IgnoreRule;
pub use worktrees::{NewWorktree, Worktree, parse_worktrees};
