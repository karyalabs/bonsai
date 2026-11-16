//! Askpass mode (ADR 3): Git runs Bonsai's own binary as `GIT_ASKPASS` to
//! answer HTTPS credential prompts for connected accounts.

use bonsai_git::ASKPASS_MODE_ENV;
use bonsai_keychain::{ACCOUNTS_ENV, MemoryStore, Secret, TokenKey, TokenStore, decode_accounts};

static PROGRAM: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Makes Git use this binary for credential prompts. Called by the app at
/// startup and never in tests, where the binary is the test runner.
pub fn enable() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = PROGRAM.set(exe);
    }
}

/// Applies askpass to `git` when [`enable`] was called.
#[must_use]
pub fn configure(git: bonsai_git::Git) -> bonsai_git::Git {
    match PROGRAM.get() {
        Some(program) => git.with_askpass(program),
        None => git,
    }
}

/// Debug builds only: `host/account=token` lines that stand in for the
/// Keychain, so tests can push without touching it. Release builds ignore it.
pub const TEST_TOKENS_ENV: &str = "BONSAI_ASKPASS_TEST_TOKENS";

/// Runs askpass mode when the process was started for it: either with
/// [`ASKPASS_MODE_ENV`] set and the prompt as the only argument (how Git
/// starts it), or as `bonsai --askpass <prompt>`. Returns the exit code, or
/// `None` to start the app normally.
#[must_use]
pub fn run_from_env() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prompt = match args.as_slice() {
        [flag, prompt] if flag == "--askpass" => prompt,
        [prompt] if std::env::var_os(ASKPASS_MODE_ENV).is_some() => prompt,
        _ => return None,
    };
    // Tests pass the accounts in the environment; the app's are in the
    // accounts file, read fresh for each prompt.
    let accounts = match std::env::var(ACCOUNTS_ENV) {
        Ok(encoded) => decode_accounts(&encoded),
        Err(_) => crate::accounts::askpass_accounts(&crate::accounts::accounts_path(
            &crate::session::data_dir(),
        )),
    };
    let store = store();
    // Declining exits non-zero with nothing on stdout, so Git reports that
    // it could not read a credential instead of using a wrong one.
    Some(
        match bonsai_keychain::answer(prompt, &accounts, store.as_ref()) {
            Some(answer) => {
                println!("{}", answer.expose());
                0
            }
            None => 1,
        },
    )
}

fn store() -> Box<dyn TokenStore> {
    if cfg!(debug_assertions)
        && let Ok(lines) = std::env::var(TEST_TOKENS_ENV)
    {
        let store = MemoryStore::default();
        for line in lines.lines() {
            if let Some((key, token)) = line.split_once('=')
                && let Some((host, account)) = key.split_once('/')
            {
                let _ = store.set(&TokenKey::new(host, account), &Secret::new(token));
            }
        }
        return Box::new(store);
    }
    bonsai_keychain::default_store()
}
