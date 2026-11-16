//! Bonsai's binary in askpass mode answers Git's credential prompts for a
//! connected account, against a local HTTP server that requires a token.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use bonsai_git::{CommandLog, CommandRecord, Git, PushRequest, Repository};
use bonsai_keychain::{ACCOUNTS_ENV, AccountRef, encode_accounts};

const TOKEN: &str = "bns_test_token_4f2a";

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Serves `root` over smart HTTP with `git http-backend`, accepting only
/// Basic auth with `x-access-token` and [`TOKEN`]. Returns the port and
/// every Authorization header seen.
fn serve(root: PathBuf) -> (u16, Arc<Mutex<Vec<Option<String>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (root, log) = (root.clone(), log.clone());
            thread::spawn(move || handle(stream, &root, &log));
        }
    });
    (port, seen)
}

fn handle(mut stream: TcpStream, root: &Path, log: &Mutex<Vec<Option<String>>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    let auth = header("authorization");
    log.lock().unwrap().push(auth.clone());
    // base64("x-access-token:" + TOKEN)
    let expected = format!("Basic {}", base64(&format!("x-access-token:{TOKEN}")));
    if auth.as_deref() != Some(expected.as_str()) {
        let _ = stream.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"test\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    }
    let mut body = Vec::new();
    if let Some(length) = header("content-length") {
        body.resize(length.parse().unwrap(), 0);
        reader.read_exact(&mut body).unwrap();
    } else if header("transfer-encoding").as_deref() == Some("chunked") {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).unwrap();
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut child = Command::new("git")
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REMOTE_USER", "x-access-token")
        .env("REQUEST_METHOD", &method)
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("CONTENT_TYPE", header("content-type").unwrap_or_default())
        .env("CONTENT_LENGTH", body.len().to_string())
        .env(
            "HTTP_CONTENT_ENCODING",
            header("content-encoding").unwrap_or_default(),
        )
        .env("GIT_PROTOCOL", header("git-protocol").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&body).unwrap();
    let output = child.wait_with_output().unwrap();
    let split = output
        .stdout
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| {
            output
                .stdout
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|i| (i, 2))
        })
        .unwrap();
    let cgi_headers = String::from_utf8_lossy(&output.stdout[..split.0]).into_owned();
    let content = &output.stdout[split.0 + split.1..];
    let mut status = "200 OK".to_owned();
    let mut response = String::new();
    for line in cgi_headers.lines() {
        if let Some(value) = line.strip_prefix("Status:") {
            value.trim().clone_into(&mut status);
        } else {
            response.push_str(line.trim_end());
            response.push_str("\r\n");
        }
    }
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status}\r\n{response}Content-Length: {}\r\nConnection: close\r\n\r\n",
            content.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(content);
}

/// Keeps every command record, as the activity log does.
#[derive(Default)]
struct Records(Mutex<Vec<CommandRecord>>);

impl CommandLog for Records {
    fn record(&self, record: &CommandRecord) {
        self.0.lock().unwrap().push(record.clone());
    }
}

fn base64(text: &str) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn pushes_over_https_with_a_connected_account_token() {
    let dir = tempfile::tempdir().unwrap();
    let served = dir.path().join("served");
    std::fs::create_dir(&served).unwrap();
    git(&served, &["init", "-q", "--bare", "-b", "main", "app.git"]);
    let (port, seen) = serve(served.clone());
    let host = format!("127.0.0.1:{port}");

    let work = dir.path().join("work");
    let located = Git::locate(None).unwrap();
    let _repository = Repository::init(&located, &work, Some("main")).unwrap();
    for (key, value) in [
        ("user.name", "Ada"),
        ("user.email", "ada@example.com"),
        ("commit.gpgsign", "false"),
        // Keep the user's own credential helpers out of the test.
        ("credential.helper", ""),
    ] {
        git(&work, &["config", key, value]);
    }
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "first"]);
    // Plain HTTP is answered only on loopback, as here.
    let url = format!("http://{host}/app.git");
    git(&work, &["remote", "add", "origin", &url]);

    let accounts = encode_accounts(&[AccountRef {
        host: host.clone(),
        account: "ada".into(),
        git_username: "x-access-token".into(),
    }]);
    let bonsai = env!("CARGO_BIN_EXE_bonsai");
    let records = Arc::new(Records::default());
    let with_askpass = |git: Git, token: &str| {
        Repository::open(
            &git.with_log(records.clone())
                .with_askpass(bonsai)
                .with_env(ACCOUNTS_ENV, accounts.clone())
                .with_env(
                    bonsai::askpass::TEST_TOKENS_ENV,
                    format!("{host}/ada={token}"),
                ),
            &work,
        )
        .unwrap()
    };

    // A wrong token is refused and Git does not hang waiting for a prompt.
    let wrong = with_askpass(located.clone(), "nope");
    let error = wrong
        .push(
            &PushRequest::new("origin", "main").set_upstream(),
            None,
            None,
        )
        .unwrap_err()
        .to_string();
    assert!(!error.contains("nope"), "{error}");

    let right = with_askpass(located, TOKEN);
    right
        .push(
            &PushRequest::new("origin", "main").set_upstream(),
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        git(&served.join("app.git"), &["rev-parse", "main"]),
        git(&work, &["rev-parse", "main"])
    );
    assert!(seen.lock().unwrap().iter().any(Option::is_some));

    // The activity log saw both pushes and neither token.
    let records = records.0.lock().unwrap();
    assert!(
        records
            .iter()
            .filter(|r| r.args.iter().any(|a| a == "push"))
            .count()
            >= 2
    );
    for record in records.iter() {
        let text = format!(
            "{} {:?} {} {}",
            record.command_line(),
            record.cwd,
            record.stdout,
            record.stderr
        );
        for secret in [TOKEN, "nope"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
}
