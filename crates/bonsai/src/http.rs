//! The HTTP client providers use: the system's `curl`, so requests follow
//! the system's certificates and proxy settings with no TLS stack in
//! Bonsai. Headers and bodies go to curl on stdin as a config file, so
//! tokens never appear in the process list.

use std::{
    io::Write as _,
    process::{Command, Stdio},
};

use bonsai_providers::{BoxFuture, HttpClient, Method, Request, Response};
use futures::channel::oneshot;

#[derive(Clone, Debug)]
pub struct CurlClient {
    program: String,
}

impl Default for CurlClient {
    fn default() -> Self {
        Self {
            program: if cfg!(target_os = "macos") {
                "/usr/bin/curl".to_owned()
            } else {
                "curl".to_owned()
            },
        }
    }
}

/// Quotes a value for a curl config file.
fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn config(request: &Request) -> Result<String, String> {
    let body = std::str::from_utf8(&request.body)
        .map_err(|_| "Request bodies must be text.".to_owned())?;
    let mut lines = vec![
        format!("url = {}", quote(&request.url)),
        "silent".to_owned(),
        "show-error".to_owned(),
        "include".to_owned(),
        "max-time = 60".to_owned(),
        // Follow redirects, such as Bitbucket's diffs. curl drops the
        // Authorization header if a redirect leaves the host.
        "location".to_owned(),
        "max-redirs = 5".to_owned(),
        "proto = \"=https,http\"".to_owned(),
        // No Expect: 100-continue round trip.
        "header = \"Expect:\"".to_owned(),
    ];
    match request.method {
        Method::Get => {}
        Method::Delete => lines.push("request = \"DELETE\"".to_owned()),
        Method::Post | Method::Put | Method::Patch => {
            let verb = match request.method {
                Method::Put => "PUT",
                Method::Patch => "PATCH",
                _ => "POST",
            };
            lines.push(format!("request = \"{verb}\""));
            lines.push(format!("data-binary = {}", quote(body)));
        }
    }
    for (name, value) in &request.headers {
        lines.push(format!("header = {}", quote(&format!("{name}: {value}"))));
    }
    Ok(lines.join("\n") + "\n")
}

/// Splits curl's `--include` output into the final response.
fn parse(output: &[u8]) -> Result<Response, String> {
    let mut rest = output;
    loop {
        let end = rest
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("The response had no headers.")?;
        let head = String::from_utf8_lossy(&rest[..end]).into_owned();
        let body = &rest[end + 4..];
        let mut lines = head.lines();
        let status: u16 = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .ok_or("The response had no status.")?;
        // Interim responses (100 Continue) and followed redirects come
        // before the real one.
        if (100..200).contains(&status)
            || ((300..400).contains(&status) && body.starts_with(b"HTTP/"))
        {
            rest = body;
            continue;
        }
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
            .collect();
        return Ok(Response {
            status,
            headers,
            body: body.to_vec(),
        });
    }
}

impl CurlClient {
    fn send_blocking(&self, request: &Request) -> Result<Response, String> {
        let config = config(request)?;
        let mut child = Command::new(&self.program)
            .args(["--disable", "--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("curl could not start: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(config.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr)
                .trim()
                .trim_start_matches("curl: ")
                .to_owned();
            return Err(if message.is_empty() {
                "The request failed.".to_owned()
            } else {
                message
            });
        }
        parse(&output.stdout)
    }
}

impl HttpClient for CurlClient {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, String>> {
        let (tx, rx) = oneshot::channel();
        let client = self.clone();
        std::thread::spawn(move || {
            let _ = tx.send(client.send_blocking(&request));
        });
        Box::pin(async move {
            rx.await
                .unwrap_or_else(|_| Err("The request stopped.".to_owned()))
        })
    }
}

/// Debug builds only: answers requests from a JSON file named by
/// `BONSAI_HTTP_REPLAY`, `{"url substring": "body", ...}`, so screenshots
/// and demos show hosted data with no network or account.
#[cfg(debug_assertions)]
pub struct Replay(Vec<(String, String)>);

#[cfg(debug_assertions)]
impl Replay {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("BONSAI_HTTP_REPLAY")?;
        let text = std::fs::read_to_string(path).ok()?;
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text).ok()?;
        Some(Self(
            map.into_iter()
                .map(|(k, v)| (k, v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
                .collect(),
        ))
    }
}

#[cfg(debug_assertions)]
impl HttpClient for Replay {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, String>> {
        let found = self
            .0
            .iter()
            .filter(|(key, _)| request.url.contains(key.as_str()))
            // The most specific key wins: `/issues/7/comments` over `/issues`.
            .max_by_key(|(key, _)| key.len())
            .map(|(_, body)| body.clone());
        Box::pin(async move {
            Ok(match found {
                Some(body) => Response {
                    status: 200,
                    headers: Vec::new(),
                    body: body.into_bytes(),
                },
                None => Response {
                    status: 404,
                    headers: Vec::new(),
                    body: b"{}".to_vec(),
                },
            })
        })
    }
}

/// The client the app uses: the system's curl, or a replay in debug builds.
#[must_use]
pub fn default_client() -> std::sync::Arc<dyn HttpClient> {
    #[cfg(debug_assertions)]
    if let Some(replay) = Replay::from_env() {
        return std::sync::Arc::new(replay);
    }
    std::sync::Arc::new(CurlClient::default())
}

/// Whether requests are replayed, so a missing token is not an error.
#[must_use]
pub fn replaying() -> bool {
    cfg!(debug_assertions) && std::env::var_os("BONSAI_HTTP_REPLAY").is_some()
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
    };

    use super::*;

    #[test]
    fn sends_headers_and_body_and_reads_the_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = Vec::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                head.push(line);
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut stream = stream;
            stream
                .write_all(b"HTTP/1.1 201 Created\r\nLink: <x>; rel=\"next\"\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
            (head, String::from_utf8(body).unwrap())
        });

        let request = Request::post(
            format!("http://127.0.0.1:{port}/repos"),
            r#"{"name":"a \"b\""}"#,
        )
        .header("Authorization", "Bearer secret-token")
        .header("Content-Type", "application/json");
        let response = futures::executor::block_on(CurlClient::default().send(request)).unwrap();
        assert_eq!(response.status, 201);
        assert_eq!(response.header("link"), Some("<x>; rel=\"next\""));
        assert_eq!(response.body, b"ok");
        let (head, body) = server.join().unwrap();
        assert!(head[0].starts_with("POST /repos HTTP/1.1"));
        assert!(
            head.iter()
                .any(|h| h.trim() == "Authorization: Bearer secret-token")
        );
        assert_eq!(body, r#"{"name":"a \"b\""}"#);
    }

    #[test]
    fn follows_redirects_on_the_same_host() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in [
                "HTTP/1.1 302 Found\r\nLocation: /diff/x\r\nContent-Length: 0\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndiff",
            ] {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = Vec::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    head.push(line.trim().to_owned());
                }
                let mut stream = stream;
                stream.write_all(reply.as_bytes()).unwrap();
                seen.push(head);
            }
            seen
        });
        let request = Request::get(format!("http://127.0.0.1:{port}/pullrequests/1/diff"))
            .header("Authorization", "Bearer t");
        let response = futures::executor::block_on(CurlClient::default().send(request)).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"diff");
        let seen = server.join().unwrap();
        assert!(seen[1][0].starts_with("GET /diff/x"));
        assert!(seen[1].iter().any(|h| h == "Authorization: Bearer t"));
    }

    #[test]
    fn reports_connection_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = futures::executor::block_on(
            CurlClient::default().send(Request::get(format!("http://127.0.0.1:{port}/"))),
        )
        .unwrap_err();
        assert_ne!(error, "");
    }

    #[test]
    fn skips_followed_redirects() {
        let response = parse(
            b"HTTP/2 302 \r\nlocation: /diff/x\r\n\r\nHTTP/2 200 \r\ncontent-type: text/plain\r\n\r\ndiff --git",
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"diff --git");
        // An unfollowed redirect is the answer.
        assert_eq!(parse(b"HTTP/2 304 \r\n\r\n").unwrap().status, 304);
    }

    #[test]
    fn skips_interim_responses() {
        let response =
            parse(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/2 404 \r\nx-a: 1\r\n\r\n{}").unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(response.header("X-A"), Some("1"));
    }
}
