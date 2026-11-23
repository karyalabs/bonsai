//! Runs the system `git` with Bonsai's environment and records each command.

use std::{
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use crate::{Error, GitVersion, Result, redact::redact};

/// How much of stdout a [`CommandRecord`] keeps for the activity log.
const RECORDED_STDOUT_LIMIT: usize = 16 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Set for the askpass program, so Bonsai's binary answers the prompt.
pub const ASKPASS_MODE_ENV: &str = "BONSAI_ASKPASS";

/// Receives a record of every Git command once it finishes.
pub trait CommandLog: Send + Sync {
    fn record(&self, record: &CommandRecord);
}

/// What the activity log shows for one Git command. Credentials in URLs are
/// redacted before the record is built.
#[derive(Clone, Debug)]
pub struct CommandRecord {
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub started_at: SystemTime,
    pub duration: Duration,
    /// `None` when the process was killed by a signal or cancelled.
    pub exit_code: Option<i32>,
    /// The first part of stdout, up to 16 KiB.
    pub stdout: String,
    pub stderr: String,
    pub cancelled: bool,
}

impl CommandRecord {
    /// Exited with status 0.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && !self.cancelled
    }

    /// The command as a shell line, quoted so it can be pasted into a
    /// terminal in the repository.
    #[must_use]
    pub fn command_line(&self) -> String {
        std::iter::once("git")
            .chain(self.args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Only reads the repository (status, log, diff and the like), so the
    /// activity log can hide it by default.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        let Some(sub) = self.args.first().map(String::as_str) else {
            return true;
        };
        let rest: Vec<&str> = self.args[1..].iter().map(String::as_str).collect();
        match sub {
            "status" | "log" | "diff" | "show" | "rev-parse" | "rev-list" | "for-each-ref"
            | "cat-file" | "ls-tree" | "ls-files" | "merge-base" | "symbolic-ref" | "version"
            | "blame" | "shortlog" | "describe" | "check-ignore" | "check-ref-format" | "var"
            | "show-ref" | "name-rev" | "count-objects" | "diff-tree" | "diff-index"
            | "diff-files" | "fast-export" | "grep" => true,
            "config" => rest
                .iter()
                .any(|a| a.starts_with("--get") || *a == "--list" || *a == "-l"),
            "stash" => matches!(rest.first(), Some(&"list" | &"show")),
            "worktree" => rest.first() == Some(&"list"),
            "reflog" => !matches!(rest.first(), Some(&"expire" | &"delete" | &"drop")),
            "remote" => {
                rest.is_empty() || matches!(rest.first(), Some(&"-v" | &"get-url" | &"show"))
            }
            "branch" | "tag" => {
                rest.is_empty()
                    || rest.iter().any(|a| {
                        matches!(
                            *a,
                            "--list"
                                | "-l"
                                | "--contains"
                                | "--merged"
                                | "--no-merged"
                                | "--format"
                        ) || a.starts_with("--format=")
                    })
            }
            _ => false,
        }
    }
}

/// Quotes one word for a POSIX shell, leaving plain words alone.
fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^".contains(&b));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Set from any thread to stop a running command; the process is killed.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The located `git` binary and how Bonsai runs it.
#[derive(Clone)]
pub struct Git {
    program: PathBuf,
    version: GitVersion,
    askpass: Option<PathBuf>,
    /// Extra environment for every command.
    env: Vec<(OsString, OsString)>,
    log: Option<Arc<dyn CommandLog>>,
}

impl std::fmt::Debug for Git {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Git")
            .field("program", &self.program)
            .field("version", &self.version)
            .field("askpass", &self.askpass)
            .finish_non_exhaustive()
    }
}

impl Git {
    /// Finds `git`: the configured path if given, otherwise `git` on `PATH`,
    /// and checks that it is at least [`GitVersion::MINIMUM`].
    pub fn locate(configured: Option<&Path>) -> Result<Self> {
        let program = configured.map_or_else(|| PathBuf::from("git"), Path::to_path_buf);
        let output = Command::new(&program)
            .arg("version")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .output()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Error::GitNotFound
                } else {
                    Error::Spawn(error)
                }
            })?;
        if !output.status.success() {
            return Err(Error::GitNotFound);
        }
        let version = GitVersion::parse(&String::from_utf8_lossy(&output.stdout))?;
        if version < GitVersion::MINIMUM {
            return Err(Error::GitTooOld {
                found: version,
                required: GitVersion::MINIMUM,
            });
        }
        Ok(Self {
            program,
            version,
            askpass: None,
            env: Vec::new(),
            log: None,
        })
    }

    #[must_use]
    pub fn version(&self) -> GitVersion {
        self.version
    }

    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Answers HTTPS credential prompts with this program (Bonsai's askpass
    /// mode, ADR 3). Git runs it with the prompt as its only argument and
    /// [`ASKPASS_MODE_ENV`] set, so Bonsai's binary knows to answer instead
    /// of opening a window.
    #[must_use]
    pub fn with_askpass(mut self, program: impl Into<PathBuf>) -> Self {
        self.askpass = Some(program.into());
        self
    }

    /// Sets an environment variable for every command, such as the accounts
    /// askpass may answer for. Values are not recorded in the activity log.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Sends a [`CommandRecord`] for every command to `log`.
    #[must_use]
    pub fn with_log(mut self, log: Arc<dyn CommandLog>) -> Self {
        self.log = Some(log);
        self
    }

    /// Starts building a command.
    pub fn command<I, S>(&self, args: I) -> GitCommand
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        GitCommand {
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
            stdin: None,
            env: Vec::new(),
            cancel: None,
            on_stderr: None,
        }
    }

    /// Runs `command` and fails with [`Error::Failed`] on a non-zero exit.
    pub fn run(&self, command: &GitCommand) -> Result<Output> {
        let output = self.run_unchecked(command)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(Error::Failed {
                record: Box::new(output.record),
            })
        }
    }

    /// Runs `command` and returns its output whatever the exit code. Only a
    /// failure to start or a cancellation is an error.
    pub fn run_unchecked(&self, command: &GitCommand) -> Result<Output> {
        let started_at = SystemTime::now();
        let started = Instant::now();
        let mut child = self.spawn(command)?;

        let stdin_writer = command.stdin.clone().and_then(|bytes| {
            let mut stdin = child.stdin.take()?;
            Some(thread::spawn(move || {
                // A command that exits early closes the pipe; that is not an error here.
                let _ = stdin.write_all(&bytes);
            }))
        });
        let stdout_reader = read_to_end(child.stdout.take(), None);
        let stderr_reader = read_to_end(child.stderr.take(), command.on_stderr.clone());

        let mut cancelled = false;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(Error::Spawn)? {
                break status;
            }
            if command
                .cancel
                .as_ref()
                .is_some_and(CancelToken::is_cancelled)
            {
                cancelled = true;
                kill_tree(&mut child);
                let status = child.wait().map_err(Error::Spawn)?;
                // A child Git was forking as the group was killed can miss
                // the signal and hold the output pipes open; the group
                // outlives its leader, so kill it again.
                kill_group(child.id());
                break status;
            }
            thread::sleep(POLL_INTERVAL);
        };

        if let Some(writer) = stdin_writer {
            let _ = writer.join();
        }
        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();

        let record = CommandRecord {
            args: command
                .args
                .iter()
                .map(|arg| redact(&arg.to_string_lossy()))
                .collect(),
            cwd: command.cwd.clone(),
            started_at,
            duration: started.elapsed(),
            exit_code: if cancelled { None } else { status.code() },
            stdout: redact(&String::from_utf8_lossy(
                &stdout[..stdout.len().min(RECORDED_STDOUT_LIMIT)],
            )),
            stderr: redact(&String::from_utf8_lossy(&stderr)),
            cancelled,
        };
        if let Some(log) = &self.log {
            log.record(&record);
        }
        if cancelled {
            return Err(Error::Cancelled);
        }
        Ok(Output {
            status,
            stdout,
            stderr,
            record,
        })
    }

    /// Starts `command` and hands back its output as it arrives, for output
    /// too long to wait for, such as a whole history. Stdin and cancellation
    /// are not used: drop the stream to stop the command.
    pub fn stream(&self, command: &GitCommand) -> Result<OutputStream> {
        let started_at = SystemTime::now();
        let started = Instant::now();
        let mut child = self.spawn(command)?;
        let stdout = child.stdout.take().map(std::io::BufReader::new);
        let stderr = read_to_end(child.stderr.take(), command.on_stderr.clone());
        Ok(OutputStream {
            child,
            stdout,
            stderr: Some(stderr),
            log: self.log.clone(),
            args: command.args.clone(),
            cwd: command.cwd.clone(),
            started_at,
            started,
            head: Vec::new(),
        })
    }

    fn spawn(&self, command: &GitCommand) -> Result<std::process::Child> {
        let mut process = Command::new(&self.program);
        process
            .args(["-c", "color.ui=never", "-c", "core.quotepath=false"])
            .args(&command.args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(if command.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own process group, so cancelling also stops hooks, credential
        // helpers and ssh that Git started (they hold the output pipes open).
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut process, 0);
        if let Some(askpass) = &self.askpass {
            process
                .env("GIT_ASKPASS", askpass)
                .env(ASKPASS_MODE_ENV, "1");
        }
        for (key, value) in &self.env {
            process.env(key, value);
        }
        if let Some(cwd) = &command.cwd {
            process.current_dir(cwd);
        }
        for (key, value) in &command.env {
            process.env(key, value);
        }

        process.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::GitNotFound
            } else {
                Error::Spawn(error)
            }
        })
    }
}

/// A running command's output, read a record at a time. Dropping it stops
/// the command if it is still running, and logs it.
pub struct OutputStream {
    child: std::process::Child,
    stdout: Option<std::io::BufReader<std::process::ChildStdout>>,
    stderr: Option<thread::JoinHandle<Vec<u8>>>,
    log: Option<Arc<dyn CommandLog>>,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    started_at: SystemTime,
    started: Instant,
    /// The first part of stdout, for the log.
    head: Vec<u8>,
}

impl std::fmt::Debug for OutputStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputStream")
            .field("args", &self.args)
            .finish_non_exhaustive()
    }
}

impl OutputStream {
    /// The next record ending in `delimiter`, without it, or `None` at the
    /// end. A command that ended in failure is an error once its output is
    /// read.
    pub fn next_record(&mut self, delimiter: u8) -> Result<Option<Vec<u8>>> {
        let Some(stdout) = self.stdout.as_mut() else {
            return Ok(None);
        };
        let mut record = Vec::new();
        std::io::BufRead::read_until(stdout, delimiter, &mut record).map_err(Error::Spawn)?;
        if self.head.len() < RECORDED_STDOUT_LIMIT {
            let room = RECORDED_STDOUT_LIMIT - self.head.len();
            self.head
                .extend_from_slice(&record[..record.len().min(room)]);
        }
        if record.is_empty() {
            self.stdout = None;
            let record = self.finish(false)?;
            return if record.succeeded() {
                Ok(None)
            } else {
                Err(Error::Failed {
                    record: Box::new(record),
                })
            };
        }
        if record.last() == Some(&delimiter) {
            record.pop();
        }
        Ok(Some(record))
    }

    /// Waits for (or with `kill`, stops) the command and logs it once.
    fn finish(&mut self, kill: bool) -> Result<CommandRecord> {
        if kill {
            kill_tree(&mut self.child);
        }
        let status = self.child.wait().map_err(Error::Spawn)?;
        if kill {
            kill_group(self.child.id());
        }
        let stderr = self
            .stderr
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        let record = CommandRecord {
            args: self
                .args
                .iter()
                .map(|arg| redact(&arg.to_string_lossy()))
                .collect(),
            cwd: self.cwd.clone(),
            started_at: self.started_at,
            duration: self.started.elapsed(),
            exit_code: if kill { None } else { status.code() },
            stdout: redact(&String::from_utf8_lossy(&self.head)),
            stderr: redact(&String::from_utf8_lossy(&stderr)),
            cancelled: kill,
        };
        if let Some(log) = self.log.take() {
            log.record(&record);
        }
        Ok(record)
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        if self.stderr.is_some() {
            self.stdout = None;
            let _ = self.finish(true);
        }
    }
}

fn kill_tree(child: &mut std::process::Child) {
    kill_group(child.id());
    let _ = child.kill();
}

/// Kills the process group `id` leads, when there is one.
fn kill_group(id: u32) {
    #[cfg(unix)]
    if let Some(group) = i32::try_from(id)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    #[cfg(not(unix))]
    let _ = id;
}

/// Reads a pipe to its end. With a sink, each line is also passed on as it
/// arrives; Git ends progress updates with `\r`, so both `\r` and `\n` end a
/// line.
fn read_to_end<R: Read + Send + 'static>(
    pipe: Option<R>,
    sink: Option<LineSink>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let Some(mut pipe) = pipe else {
            return bytes;
        };
        let Some(sink) = sink else {
            let _ = pipe.read_to_end(&mut bytes);
            return bytes;
        };
        let mut chunk = [0_u8; 4096];
        let mut line_start = 0;
        loop {
            let read = match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            bytes.extend_from_slice(&chunk[..read]);
            while let Some(end) = bytes[line_start..]
                .iter()
                .position(|b| *b == b'\r' || *b == b'\n')
            {
                let line = &bytes[line_start..line_start + end];
                if !line.is_empty() {
                    (sink.0)(&String::from_utf8_lossy(line));
                }
                line_start += end + 1;
            }
        }
        if line_start < bytes.len() {
            (sink.0)(&String::from_utf8_lossy(&bytes[line_start..]));
        }
        bytes
    })
}

/// Receives stderr line by line while a command runs.
#[derive(Clone)]
pub struct LineSink(Arc<dyn Fn(&str) + Send + Sync>);

impl std::fmt::Debug for LineSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LineSink")
    }
}

/// One Git invocation, built with [`Git::command`].
#[derive(Clone, Debug)]
pub struct GitCommand {
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    stdin: Option<Vec<u8>>,
    env: Vec<(OsString, OsString)>,
    cancel: Option<CancelToken>,
    on_stderr: Option<LineSink>,
}

impl GitCommand {
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Calls `sink` with each stderr line as it arrives, such as `--progress`
    /// updates. Stderr is still returned in full.
    #[must_use]
    pub fn on_stderr(mut self, sink: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.on_stderr = Some(LineSink(Arc::new(sink)));
        self
    }
}

/// A finished command's raw output.
#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub record: CommandRecord,
}

impl Output {
    /// Stdout as UTF-8, replacing invalid sequences.
    #[must_use]
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}
