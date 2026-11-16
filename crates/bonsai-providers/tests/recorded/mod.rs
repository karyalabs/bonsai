//! A client that replays recorded responses and keeps the requests it got.

#![allow(dead_code)]

use std::sync::Mutex;

use bonsai_providers::{BoxFuture, HttpClient, Method, Request, Response};

pub struct Recorded {
    /// Expected method and URL, and the response to give.
    script: Mutex<Vec<(Method, String, Response)>>,
    pub seen: Mutex<Vec<Request>>,
}

impl Recorded {
    pub fn new(script: Vec<(Method, &str, Response)>) -> Self {
        Self {
            script: Mutex::new(
                script
                    .into_iter()
                    .rev()
                    .map(|(m, u, r)| (m, u.to_owned(), r))
                    .collect(),
            ),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }

    pub fn finished(&self) -> bool {
        self.script.lock().unwrap().is_empty()
    }
}

impl HttpClient for &Recorded {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, String>> {
        let next = self.script.lock().unwrap().pop();
        self.seen.lock().unwrap().push(request.clone());
        Box::pin(async move {
            let (method, url, response) = next.expect("no more recorded responses");
            assert_eq!(
                (request.method, request.url.as_str()),
                (method, url.as_str())
            );
            Ok(response)
        })
    }
}

pub fn json(status: u16, body: &str) -> Response {
    Response {
        status,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: body.as_bytes().to_vec(),
    }
}

pub fn with_header(mut response: Response, name: &str, value: &str) -> Response {
    response.headers.push((name.into(), value.into()));
    response
}

/// Fails every request at the transport level.
pub struct Offline;

impl HttpClient for Offline {
    fn send(&self, _: Request) -> BoxFuture<'_, Result<Response, String>> {
        Box::pin(async { Err("connection refused".to_owned()) })
    }
}
