//! HTTP requests from a WASM plugin that never block the server.
//!
//! A plugin call runs inside the server's tick or packet handling, so waiting for the network
//! there freezes the game for everyone. This client never waits: `start` hands the request to
//! the host (which does DNS, TLS and the transfer on its own threads) and returns at once;
//! `Pending::poll`, called from a tick task, picks up whatever has arrived since.
//!
//! The plugin must ask for the `http.outbound` permission in its metadata (and `network.dns`
//! for host names), and the server owner must allow them in `pumpkin.toml`.
use std::time::{Duration, Instant};

use wasip2::http::outgoing_handler;
use wasip2::http::types::{
    Fields, FutureIncomingResponse, IncomingBody, IncomingResponse, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use wasip2::io::poll::Pollable;
use wasip2::io::streams::{InputStream, StreamError};

/// Largest response body kept; the rest of a bigger one is dropped and the request fails.
pub const MAX_BODY: usize = 512 * 1024;

pub struct Request {
    /// GET, POST, PUT, PATCH or DELETE.
    pub method: &'static str,
    /// `http://` or `https://` URL.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Whole-request limit, connect to last byte.
    pub timeout: Duration,
    /// Largest answer taken (bytes); a bigger one fails. `MAX_BODY` unless set.
    pub max_body: usize,
    /// Do not keep the answer, only its SHA-1 and size (a file far too large to hold, like a
    /// resource pack). `max_body` still bounds how much is read.
    pub digest: bool,
}

impl Request {
    pub fn get(url: &str) -> Self {
        Self { method: "GET", url: url.to_owned(), headers: Vec::new(), body: Vec::new(), timeout: Duration::from_secs(10), max_body: MAX_BODY, digest: false }
    }

    pub fn post_json(url: &str, json: String) -> Self {
        Self {
            method: "POST",
            url: url.to_owned(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: json.into_bytes(),
            timeout: Duration::from_secs(10),
            max_body: MAX_BODY,
            digest: false,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Empty for a `digest` request.
    pub body: Vec<u8>,
    /// Bytes of the answer (also when only the digest was kept).
    pub size: u64,
    /// SHA-1 of the answer, for a `digest` request.
    pub digest: Option<[u8; 20]>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// First header with this name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Scheme, authority and path of a URL. Plain parsing: no userinfo, no fragments.
pub fn split_url(url: &str) -> Option<(bool, &str, &str)> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains(['@', ' ', '#']) {
        return None;
    }
    Some((https, authority, path))
}

/// The body being read. Field order matters: a child handle has to go before its parent.
struct Reading {
    stream: InputStream,
    body: IncomingBody,
    response: IncomingResponse,
    status: u16,
    headers: Vec<(String, String)>,
    buf: Vec<u8>,
    hasher: Option<crate::sha1::Sha1>,
    size: u64,
}

/// A `digest` request reads at most this much, and for at most this long, in one `poll`: a fast
/// download of a big file must not take a whole tick.
const DIGEST_SLICE_BYTES: u64 = 4 * 1024 * 1024;
const DIGEST_SLICE: Duration = Duration::from_millis(3);

/// A request on its way. Drop it to give up on it.
pub struct Pending {
    reading: Option<Reading>,
    pollable: Option<Pollable>,
    future: Option<FutureIncomingResponse>,
    deadline: Instant,
    max_body: usize,
    digest: bool,
}

/// Send a request. Returns straight away; the answer comes through `Pending::poll`.
pub fn start(request: Request) -> Result<Pending, String> {
    let (https, authority, path) = split_url(&request.url).ok_or("bad url")?;
    let fields = Fields::new();
    for (name, value) in &request.headers {
        fields.append(name, value.as_bytes()).map_err(|e| format!("header {name}: {e:?}"))?;
    }
    if !request.body.is_empty() {
        let _ = fields.set("content-length", &[request.body.len().to_string().into_bytes()]);
    }
    let out = OutgoingRequest::new(fields);
    let method = match request.method {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "PATCH" => Method::Patch,
        "DELETE" => Method::Delete,
        other => return Err(format!("method {other}")),
    };
    out.set_method(&method).map_err(|()| "method")?;
    out.set_scheme(Some(if https { &Scheme::Https } else { &Scheme::Http })).map_err(|()| "scheme")?;
    out.set_authority(Some(authority)).map_err(|()| "authority")?;
    out.set_path_with_query(Some(if path.starts_with('?') { "/" } else { path })).map_err(|()| "path")?;
    let nanos = request.timeout.as_nanos().min(u128::from(u64::MAX)) as u64;
    let options = RequestOptions::new();
    let _ = options.set_connect_timeout(Some(nanos));
    let _ = options.set_first_byte_timeout(Some(nanos));
    let _ = options.set_between_bytes_timeout(Some(nanos));

    let body = out.body().map_err(|()| "body")?;
    let future = outgoing_handler::handle(out, Some(options)).map_err(|e| format!("{e:?}"))?;
    // The body goes into the host's buffer; small bodies (what plugins send) never have to wait
    {
        let stream = body.write().map_err(|()| "body stream")?;
        let mut rest = request.body.as_slice();
        while !rest.is_empty() {
            let room = stream.check_write().map_err(|e| format!("write: {e:?}"))? as usize;
            if room == 0 {
                return Err("request body too large to send without waiting".into());
            }
            let (now, later) = rest.split_at(room.min(rest.len()));
            stream.write(now).map_err(|e| format!("write: {e:?}"))?;
            rest = later;
        }
        let _ = stream.flush();
    }
    OutgoingBody::finish(body, None).map_err(|e| format!("finish: {e:?}"))?;
    let pollable = future.subscribe();
    Ok(Pending { reading: None, pollable: Some(pollable), future: Some(future), deadline: Instant::now() + request.timeout, max_body: request.max_body, digest: request.digest })
}

impl Pending {
    /// `None` while the request is still under way. Never waits.
    pub fn poll(&mut self) -> Option<Result<Response, String>> {
        if Instant::now() > self.deadline {
            self.close();
            return Some(Err("timeout".into()));
        }
        if self.reading.is_none() {
            // Asking "ready?" is what lets the host move the request forward
            if !self.pollable.as_ref()?.ready() {
                return None;
            }
            let got = self.future.as_ref()?.get()?;
            self.pollable = None;
            let response = match got {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    self.close();
                    return Some(Err(format!("{e:?}")));
                }
                Err(()) => {
                    self.close();
                    return Some(Err("response already taken".into()));
                }
            };
            let status = response.status();
            let headers = response.headers().entries().into_iter().map(|(k, v)| (k, String::from_utf8_lossy(&v).into_owned())).collect();
            let (body, stream) = match response.consume().and_then(|b| b.stream().map(|s| (b, s))) {
                Ok(pair) => pair,
                Err(()) => {
                    self.close();
                    return Some(Err("no response body".into()));
                }
            };
            self.reading = Some(Reading { stream, body, response, status, headers, buf: Vec::new(), hasher: self.digest.then(crate::sha1::Sha1::new), size: 0 });
        }
        let max_body = self.max_body as u64;
        let (began, mut taken) = (Instant::now(), 0u64);
        let r = self.reading.as_mut()?;
        loop {
            match r.stream.read(64 * 1024) {
                // Nothing more right now: come back next tick
                Ok(chunk) if chunk.is_empty() => return None,
                Ok(chunk) => {
                    r.size += chunk.len() as u64;
                    if r.size > max_body {
                        self.close();
                        return Some(Err("response too large".into()));
                    }
                    match r.hasher.as_mut() {
                        Some(h) => {
                            h.update(&chunk);
                            taken += chunk.len() as u64;
                            if taken >= DIGEST_SLICE_BYTES || began.elapsed() >= DIGEST_SLICE {
                                return None;
                            }
                        }
                        None => r.buf.extend_from_slice(&chunk),
                    }
                }
                Err(StreamError::Closed) => {
                    let mut done = self.reading.take()?;
                    let digest = done.hasher.take().map(crate::sha1::Sha1::finish);
                    let out = Response { status: done.status, headers: done.headers, body: std::mem::take(&mut done.buf), size: done.size, digest };
                    drop(done.stream);
                    drop(IncomingBody::finish(done.body));
                    drop(done.response);
                    self.close();
                    return Some(Ok(out));
                }
                Err(e) => {
                    self.close();
                    return Some(Err(format!("read: {e:?}")));
                }
            }
        }
    }

    /// Wait for the answer, at most `max`. Blocks the server while it waits: only for the last
    /// message while the server is shutting down, never during play.
    pub fn wait(mut self, max: Duration) -> Result<Response, String> {
        let end = Instant::now() + max;
        loop {
            if let Some(result) = self.poll() {
                return result;
            }
            if Instant::now() >= end {
                return Err("timeout".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Let go of every host handle, children first.
    fn close(&mut self) {
        if let Some(r) = self.reading.take() {
            drop(r.stream);
            drop(r.body);
            drop(r.response);
        }
        self.pollable = None;
        self.future = None;
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.close();
    }
}
