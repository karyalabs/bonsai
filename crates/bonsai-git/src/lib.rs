//! Typed Git reads and operations for Bonsai.
//!
//! Every mutation and every network call runs the system `git` through
//! [`Git`], so hooks, signing, credential helpers and configuration behave as
//! they do in the user's terminal (ADR 2). Views never spawn processes; they go
//! through [`Repository`].

mod clone;
mod error;
mod path;
mod redact;
mod remote;
mod repository;
mod runner;
mod snapshot;
mod version;

pub use clone::{CloneProgress, clone_name};
pub use error::{Error, Result};
pub use remote::RemoteFailure;
pub use repository::Repository;
pub use runner::{CancelToken, CommandLog, CommandRecord, Git, GitCommand, Output};
pub use snapshot::{
    Branch, Change, ChangeKind, ConflictKind, EntryStatus, Head, InProgress, Oid, Remote,
    RemoteBranch, Snapshot, Stash, Tag, Tracking,
};
pub use version::GitVersion;
