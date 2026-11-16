//! macOS Keychain token storage and the askpass helper for Bonsai (ADR 3).
//!
//! Tokens live in the Keychain, keyed by provider host and account. Git
//! reaches them through `GIT_ASKPASS`: Bonsai's own binary, started with
//! `--askpass`, answers a credential prompt for a host that has a connected
//! account and declines every other prompt, so Git falls back to the user's
//! own setup.

mod askpass;
mod prompt;
mod store;

pub use askpass::{ACCOUNTS_ENV, AccountRef, answer, decode_accounts, encode_accounts};
pub use prompt::{Prompt, PromptKind};
#[cfg(target_os = "macos")]
pub use store::Keychain;
pub use store::{Error, MemoryStore, Result, SERVICE, Secret, TokenKey, TokenStore, default_store};
