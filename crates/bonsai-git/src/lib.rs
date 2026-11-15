//! Typed Git reads and operations for Bonsai.
//!
//! Every mutation and every network call runs the system `git` through
//! [`Git`], so hooks, signing, credential helpers and configuration behave as
//! they do in the user's terminal (ADR 2). Views never spawn processes; they go
//! through [`Repository`].

mod error;
mod redact;
mod repository;
mod runner;
mod version;

pub use error::{Error, Result};
pub use repository::Repository;
pub use runner::{CancelToken, CommandLog, CommandRecord, Git, GitCommand, Output};
pub use version::GitVersion;
