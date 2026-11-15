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
    /// mode, ADR 3).
    #[must_use]
    pub fn with_askpass(mut self, program: impl Into<PathBuf>) -> Self {
        self.askpass = Some(program.into());
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
            process.env("GIT_ASKPASS", askpass);
        }
        if let Some(cwd) = &command.cwd {
            process.current_dir(cwd);
        }
        for (key, value) in &command.env {
            process.env(key, value);
        }

        let started_at = SystemTime::now();
        let started = Instant::now();
        let mut child = process.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::GitNotFound
            } else {
                Error::Spawn(error)
            }
        })?;

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
                break child.wait().map_err(Error::Spawn)?;
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
}

fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Some(group) = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
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
