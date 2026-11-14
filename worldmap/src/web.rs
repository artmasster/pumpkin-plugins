//! The live map page: a very small HTTP/1.1 server driven from the server tick.
//!
//! A WASM plugin has one thread and no async runtime, so nothing here may block: the listener and
//! every connection are non-blocking, and [`Web::poll`] does what can be done right now and
//! returns. Only `GET` and `HEAD` exist, a request has no body, and there is no TLS (the page is
//! public and read-only, like the built-in web server of squaremap, Dynmap or BlueMap).
//!
//! Plain Rust: the tests run it natively against real sockets on loopback.
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::store::valid_dir;

/// Connections held at once; a browser opens up to six.
pub const MAX_CONNS: usize = 128;
/// ... and from one address (people behind one router or one mobile carrier share theirs).
pub const MAX_PER_IP: usize = 48;
/// A request (line and headers) larger than this is refused.
pub const MAX_HEAD: usize = 8192;
/// A connection must send a whole request within this long of becoming idle.
pub const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// ... and take at least a byte of the answer this often.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ACCEPTS: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// `HEAD`: answer without the body.
    pub head: bool,
    /// `/tiles/overworld/0_-1.json`
    pub path: String,
    /// What follows `?`, as sent.
    pub query: String,
    /// `If-None-Match`
    pub etag: Option<String>,
    pub keep_alive: bool,
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    /// The request is not all here yet.
    More,
    /// Not a request we answer: reply with this status and close.
    Bad(u16),
    /// A request and the bytes it took.
    Ready(Request, usize),
}

/// Read one request from the start of `buf`.
pub fn parse(buf: &[u8]) -> Parsed {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() >= MAX_HEAD { Parsed::Bad(431) } else { Parsed::More };
    };
    if end > MAX_HEAD {
        return Parsed::Bad(431);
    }
    let Ok(text) = std::str::from_utf8(&buf[..end]) else { return Parsed::Bad(400) };
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(target), Some(version), None) = (first.next(), first.next(), first.next(), first.next()) else { return Parsed::Bad(400) };
    if !version.starts_with("HTTP/1.") {
        return Parsed::Bad(400);
    }
    let head = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return Parsed::Bad(405),
    };
    if !target.starts_with('/') || target.len() > 2048 {
        return Parsed::Bad(400);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut req = Request { head, path: path.to_owned(), query: query.to_owned(), etag: None, keep_alive: version == "HTTP/1.1" };
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { return Parsed::Bad(400) };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "connection" => {
                let v = value.to_ascii_lowercase();
                if v.contains("close") {
                    req.keep_alive = false;
                } else if v.contains("keep-alive") {
                    req.keep_alive = true;
                }
            }
            "if-none-match" => req.etag = Some(value.to_owned()),
            // No request of ours has a body, and one that is left unread would be taken for the next request
            "content-length" if value != "0" => return Parsed::Bad(400),
            "transfer-encoding" => return Parsed::Bad(400),
            _ => {}
        }
    }
    Parsed::Ready(req, end + 4)
}

/// What a path asks for.
#[derive(Debug, PartialEq)]
pub enum Route {
    Page,
    Index,
    Players,
    /// A tile of the dimension folder `dir`.
    Tile { dir: String, x: i32, z: i32 },
    Unknown,
}

/// Paths are matched whole: nothing from the request ever becomes part of a file path except a
/// dimension folder name that passed [`valid_dir`] and two numbers.
pub fn route(path: &str) -> Route {
    match path {
        "/" | "/index.html" => return Route::Page,
        "/index.json" => return Route::Index,
        "/players.json" => return Route::Players,
        _ => {}
    }
    let Some(rest) = path.strip_prefix("/tiles/") else { return Route::Unknown };
    let Some((dir, name)) = rest.split_once('/') else { return Route::Unknown };
    let Some((x, z)) = name.strip_suffix(".json").and_then(|n| n.split_once('_')) else { return Route::Unknown };
    let (Ok(x), Ok(z)) = (x.parse::<i32>(), z.parse::<i32>()) else { return Route::Unknown };
    // Only the one spelling the store writes (`+1`, `01` and the like are not it)
    if !valid_dir(dir) || format!("{x}_{z}.json") != name {
        return Route::Unknown;
    }
    Route::Tile { dir: dir.to_owned(), x, z }
}

pub const NO_CACHE: &str = "no-cache";
pub const NO_STORE: &str = "no-store";
/// For a URL that names a version (`?v=<updated>`): never asked for again.
pub const FOREVER: &str = "public, max-age=31536000, immutable";

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    /// `Content-Type`
    pub kind: &'static str,
    pub body: Arc<Vec<u8>>,
    /// With quotes: `"1790000000-51234"`. A request that already has it gets `304`.
    pub etag: Option<String>,
    /// `Cache-Control`
    pub cache: &'static str,
    /// More header lines, each ending in `\r\n`.
    pub extra: &'static str,
}

impl Response {
    pub fn text(status: u16, body: &str) -> Response {
        Response { status, kind: "text/plain; charset=utf-8", body: Arc::new(body.as_bytes().to_vec()), etag: None, cache: NO_STORE, extra: "" }
    }

    pub fn not_found() -> Response {
        Response::text(404, "not found\n")
    }

    pub fn json(body: Arc<Vec<u8>>, etag: Option<String>, cache: &'static str) -> Response {
        Response { status: 200, kind: "application/json", body, etag, cache, extra: "" }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        304 => "Not Modified",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// Does `If-None-Match` name this entity tag?
pub fn etag_matches(sent: &str, etag: &str) -> bool {
    sent.split(',').any(|t| {
        let t = t.trim();
        t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag
    })
}

/// The header block of an answer, and whether its body follows.
pub fn head_bytes(resp: &Response, req: Option<&Request>, keep_alive: bool) -> (Vec<u8>, bool) {
    let cached = resp.status == 200 && resp.etag.as_deref().is_some_and(|e| req.and_then(|r| r.etag.as_deref()).is_some_and(|sent| etag_matches(sent, e)));
    let status = if cached { 304 } else { resp.status };
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    if !cached {
        head.push_str(&format!("Content-Type: {}\r\nContent-Length: {}\r\n", resp.kind, resp.body.len()));
    }
    if let Some(etag) = &resp.etag {
        head.push_str(&format!("ETag: {etag}\r\n"));
    }
    if status == 405 {
        head.push_str("Allow: GET, HEAD\r\n");
    }
    head.push_str(&format!(
        "Cache-Control: {}\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n{}Connection: {}\r\n\r\n",
        resp.cache,
        resp.extra,
        if keep_alive { "keep-alive" } else { "close" }
    ));
    (head.into_bytes(), !cached && !req.is_some_and(|r| r.head))
}

struct Out {
    head: Vec<u8>,
    body: Option<Arc<Vec<u8>>>,
    sent: usize,
}

struct Conn {
    stream: TcpStream,
    ip: IpAddr,
    buf: Vec<u8>,
    out: Option<Out>,
    /// When it was accepted, finished an answer or last took bytes of one.
    last: Instant,
    /// Close when the answer is out.
    close: bool,
    /// The other side has stopped sending.
    eof: bool,
}

/// `Ok(true)` when the answer is all out, `Ok(false)` when the socket takes no more for now.
fn write_out(stream: &mut TcpStream, out: &mut Out, wrote: &mut u64) -> std::io::Result<bool> {
    loop {
        let (src, off) = if out.sent < out.head.len() {
            (&out.head[..], out.sent)
        } else if let Some(body) = &out.body {
            (&body[..], out.sent - out.head.len())
        } else {
            return Ok(true);
        };
        if off >= src.len() {
            return Ok(true);
        }
        match stream.write(&src[off..]) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => {
                out.sent += n;
                *wrote += n as u64;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

pub struct Web {
    listener: TcpListener,
    pub port: u16,
    conns: Vec<Conn>,
    /// Since the start: requests answered, connections turned away, bytes sent.
    pub served: u64,
    pub refused: u64,
    pub sent: u64,
}

impl Web {
    /// Listen on every IPv4 address of the server at `port`.
    pub fn bind(port: u16) -> std::io::Result<Web> {
        Web::on(TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))?)
    }

    pub fn on(listener: TcpListener) -> std::io::Result<Web> {
        listener.set_nonblocking(true)?;
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        Ok(Web { listener, port, conns: Vec::new(), served: 0, refused: 0, sent: 0 })
    }

    pub fn open(&self) -> usize {
        self.conns.len()
    }

    /// Make room by closing the connection (of `ip`, or of anyone) that has been idle the longest.
    /// False when every one is busy.
    fn drop_idle(&mut self, ip: Option<IpAddr>) -> bool {
        let idle = self.conns.iter().enumerate().filter(|(_, c)| c.out.is_none() && c.buf.is_empty() && ip.is_none_or(|ip| c.ip == ip)).min_by_key(|(_, c)| c.last).map(|(i, _)| i);
        match idle {
            Some(i) => {
                self.conns.swap_remove(i);
                true
            }
            None => false,
        }
    }

    fn accept(&mut self, now: Instant) {
        for _ in 0..MAX_ACCEPTS {
            let (stream, peer) = match self.listener.accept() {
                Ok(pair) => pair,
                // Nobody is waiting, or the listener is in trouble: either way, try again next tick
                Err(_) => break,
            };
            let ip = peer.ip();
            let room = (self.conns.iter().filter(|c| c.ip == ip).count() < MAX_PER_IP || self.drop_idle(Some(ip))) && (self.conns.len() < MAX_CONNS || self.drop_idle(None));
            if !room || stream.set_nonblocking(true).is_err() {
                // Dropping the stream closes it
                self.refused += 1;
                continue;
            }
            self.conns.push(Conn { stream, ip, buf: Vec::new(), out: None, last: now, close: false, eof: false });
        }
    }

    /// Do everything that can be done without waiting: take new connections, send what sockets
    /// will take, and answer complete requests with `handle`. After `deadline` (and at least one
    /// answer) the remaining requests wait for the next call.
    pub fn poll(&mut self, now: Instant, deadline: Instant, handle: &mut dyn FnMut(&Request) -> Response) {
        let mut handled = 0u32;
        if !self.conns.is_empty() {
            // Whoever was first last time is last now
            self.conns.rotate_left(1);
        }
        // The ones we have first: those that have closed make room for the new ones
        self.turn(0, now, deadline, &mut handled, handle);
        // A request usually arrives together with its connection: it is answered in this same call
        let fresh = self.conns.len();
        self.accept(now);
        self.turn(fresh, now, deadline, &mut handled, handle);
    }

    /// Give every connection from position `from` on its turn.
    fn turn(&mut self, from: usize, now: Instant, deadline: Instant, handled: &mut u32, handle: &mut dyn FnMut(&Request) -> Response) {
        let mut i = from;
        while i < self.conns.len() {
            let (mut answered, mut wrote) = (0u64, 0u64);
            let keep = step(&mut self.conns[i], now, deadline, handled, &mut answered, &mut wrote, handle);
            self.served += answered;
            self.sent += wrote;
            if keep {
                i += 1;
            } else {
                self.conns.swap_remove(i);
            }
        }
    }
}

/// One connection's turn. False = close it.
fn step(c: &mut Conn, now: Instant, deadline: Instant, handled: &mut u32, answered: &mut u64, wrote: &mut u64, handle: &mut dyn FnMut(&Request) -> Response) -> bool {
    loop {
        if let Some(out) = c.out.as_mut() {
            let before = out.sent;
            match write_out(&mut c.stream, out, wrote) {
                Ok(true) => {
                    if c.close {
                        return false;
                    }
                    c.out = None;
                    c.last = now;
                }
                Ok(false) => {
                    if out.sent > before {
                        c.last = now;
                    }
                    return now.saturating_duration_since(c.last) < WRITE_TIMEOUT;
                }
                Err(_) => return false,
            }
        }
        while !c.eof && c.buf.len() < MAX_HEAD {
            let mut chunk = [0u8; 2048];
            let room = (MAX_HEAD - c.buf.len()).min(chunk.len());
            match c.stream.read(&mut chunk[..room]) {
                Ok(0) => c.eof = true,
                Ok(n) => c.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        match parse(&c.buf) {
            Parsed::More => return !c.eof && now.saturating_duration_since(c.last) < READ_TIMEOUT,
            Parsed::Bad(status) => {
                let (head, _) = head_bytes(&Response::text(status, ""), None, false);
                c.buf.clear();
                c.eof = true;
                c.close = true;
                c.out = Some(Out { head, body: None, sent: 0 });
            }
            Parsed::Ready(req, used) => {
                if *handled > 0 && Instant::now() >= deadline {
                    return true;
                }
                c.buf.drain(..used);
                let resp = handle(&req);
                *handled += 1;
                *answered += 1;
                let (head, with_body) = head_bytes(&resp, Some(&req), req.keep_alive);
                c.close = !req.keep_alive;
                c.out = Some(Out { head, body: with_body.then_some(resp.body), sent: 0 });
                c.last = now;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(text: &str) -> Request {
        match parse(text.as_bytes()) {
            Parsed::Ready(r, used) => {
                assert_eq!(used, text.len());
                r
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn requests_are_read() {
        let r = get("GET /tiles/overworld/0_-1.json?v=17 HTTP/1.1\r\nHost: x:1\r\nIf-None-Match: \"a\"\r\n\r\n");
        assert_eq!(r, Request { head: false, path: "/tiles/overworld/0_-1.json".into(), query: "v=17".into(), etag: Some("\"a\"".into()), keep_alive: true });
        assert!(get("HEAD / HTTP/1.1\r\n\r\n").head);
        assert!(!get("GET / HTTP/1.0\r\n\r\n").keep_alive);
        assert!(get("GET / HTTP/1.0\r\nConnection: Keep-Alive\r\n\r\n").keep_alive);
        assert!(!get("GET / HTTP/1.1\r\nconnection: close\r\n\r\n").keep_alive);
        // Two requests in one buffer: only the first is taken
        let two = "GET /a HTTP/1.1\r\n\r\nGET /b HTTP/1.1\r\n\r\n";
        assert!(matches!(parse(two.as_bytes()), Parsed::Ready(r, 19) if r.path == "/a"));
    }

    #[test]
    fn other_things_are_refused() {
        assert_eq!(parse(b"GET / HTTP/1.1\r\nHost: x"), Parsed::More);
        assert_eq!(parse(b""), Parsed::More);
        assert_eq!(parse(b"POST / HTTP/1.1\r\n\r\n"), Parsed::Bad(405));
        assert_eq!(parse(b"GET / HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello"), Parsed::Bad(400));
        assert_eq!(parse(b"GET / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"), Parsed::Bad(400));
        assert_eq!(parse(b"GET http://x/ HTTP/1.1\r\n\r\n"), Parsed::Bad(400));
        assert_eq!(parse(b"GET / HTTP/2\r\n\r\n"), Parsed::Bad(400));
        assert_eq!(parse(b"GET /  HTTP/1.1\r\n\r\n"), Parsed::Bad(400));
        assert_eq!(parse(b"GET / HTTP/1.1\r\nno colon\r\n\r\n"), Parsed::Bad(400));
        assert_eq!(parse(b"\xff\xfe / HTTP/1.1\r\n\r\n"), Parsed::Bad(400));
        assert_eq!(parse(&vec![b'a'; MAX_HEAD]), Parsed::Bad(431));
        assert_eq!(parse(&vec![b'a'; MAX_HEAD - 1]), Parsed::More);
    }

    #[test]
    fn paths_never_leave_the_map() {
        assert_eq!(route("/"), Route::Page);
        assert_eq!(route("/index.html"), Route::Page);
        assert_eq!(route("/index.json"), Route::Index);
        assert_eq!(route("/players.json"), Route::Players);
        assert_eq!(route("/tiles/overworld/0_-1.json"), Route::Tile { dir: "overworld".into(), x: 0, z: -1 });
        assert_eq!(route("/tiles/the_nether/-12_340.json"), Route::Tile { dir: "the_nether".into(), x: -12, z: 340 });
        for bad in [
            "/tiles/../config.toml",
            "/tiles/overworld/../../config.toml",
            "/tiles/overworld/0_0.json/..",
            "/tiles/over world/0_0.json",
            "/tiles/Overworld/0_0.json",
            "/tiles//0_0.json",
            "/tiles/overworld/0_0",
            "/tiles/overworld/+1_0.json",
            "/tiles/overworld/01_0.json",
            "/tiles/overworld/0_0_0.json",
            "/tiles/overworld/99999999999_0.json",
            "/tiles/overworld/a/0_0.json",
            "/config.toml",
            "/status.json",
            "/map.html",
            "//",
            "/index.json/",
            "/%2e%2e/config.toml",
            "",
        ] {
            assert_eq!(route(bad), Route::Unknown, "{bad}");
        }
    }

    #[test]
    fn headers_say_what_follows() {
        let body = Arc::new(b"{}".to_vec());
        let resp = Response::json(body, Some("\"7-2\"".into()), NO_CACHE);
        let req = |etag: Option<&str>, head| Request { head, path: "/".into(), query: String::new(), etag: etag.map(str::to_owned), keep_alive: true };
        let (head, with_body) = head_bytes(&resp, Some(&req(None, false)), true);
        let head = String::from_utf8(head).unwrap();
        assert!(with_body && head.starts_with("HTTP/1.1 200 OK\r\n") && head.ends_with("Connection: keep-alive\r\n\r\n"), "{head}");
        assert!(head.contains("Content-Length: 2\r\n") && head.contains("ETag: \"7-2\"\r\n") && head.contains("Cache-Control: no-cache\r\n") && head.contains("nosniff"));
        // The browser already has it
        for sent in ["\"7-2\"", "W/\"7-2\"", "\"1-1\", \"7-2\"", "*"] {
            let (head, with_body) = head_bytes(&resp, Some(&req(Some(sent), false)), true);
            assert!(!with_body && head.starts_with(b"HTTP/1.1 304 Not Modified\r\n"), "{sent}");
            assert!(!String::from_utf8(head).unwrap().contains("Content-Length"));
        }
        assert!(head_bytes(&resp, Some(&req(Some("\"7-3\""), false)), true).1);
        // HEAD: the same headers, no body
        let (head, with_body) = head_bytes(&resp, Some(&req(None, true)), false);
        assert!(!with_body && String::from_utf8(head).unwrap().contains("Content-Length: 2\r\nETag") );
        // An error is never a 304
        let (head, _) = head_bytes(&Response::text(405, ""), None, false);
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("HTTP/1.1 405 Method Not Allowed\r\n") && head.contains("Allow: GET, HEAD\r\n") && head.ends_with("Connection: close\r\n\r\n"));
    }

    /// A server on a free loopback port, polled until `done` says so (or five seconds pass).
    fn serve(web: &mut Web, mut done: impl FnMut() -> bool) {
        let big = Arc::new(vec![b'm'; 3_000_000]);
        let began = Instant::now();
        while !done() {
            assert!(began.elapsed() < Duration::from_secs(5), "the client never finished");
            let now = Instant::now();
            web.poll(now, now + Duration::from_millis(2), &mut |req| match req.path.as_str() {
                "/big" => Response::json(big.clone(), Some("\"big\"".into()), NO_CACHE),
                "/small" => Response::text(200, "hi"),
                _ => Response::not_found(),
            });
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn client(port: u16, send: &'static [u8]) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s.write_all(send).unwrap();
            let mut all = Vec::new();
            s.read_to_end(&mut all).unwrap();
            all
        })
    }

    #[test]
    fn it_serves_real_sockets() {
        let mut web = Web::on(TcpListener::bind("127.0.0.1:0").unwrap()).unwrap();
        let port = web.port;
        // Keep-alive, two requests sent at once, a body far larger than a socket buffer, then close
        let a = client(port, b"GET /small HTTP/1.1\r\n\r\nGET /big HTTP/1.1\r\nConnection: close\r\n\r\n");
        let b = client(port, b"HEAD /big HTTP/1.0\r\n\r\n");
        let c = client(port, b"GET /big HTTP/1.1\r\nIf-None-Match: \"big\"\r\nConnection: close\r\n\r\n");
        let d = client(port, b"DELETE /small HTTP/1.1\r\n\r\n");
        let e = client(port, b"GET /nothing HTTP/1.0\r\n\r\n");
        serve(&mut web, || a.is_finished() && b.is_finished() && c.is_finished() && d.is_finished() && e.is_finished());
        let a = a.join().unwrap();
        let text = String::from_utf8_lossy(&a[..400]).into_owned();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n") && text.contains("Connection: keep-alive\r\n\r\nhiHTTP/1.1 200 OK\r\n"), "{text}");
        assert!(a.ends_with(&[b'm'; 64]) && a.len() > 3_000_000 && a.len() < 3_001_000, "{}", a.len());
        assert_eq!(a.iter().filter(|b| **b == b'm').count(), 3_000_000);
        let b = String::from_utf8(b.join().unwrap()).unwrap();
        assert!(b.starts_with("HTTP/1.1 200 OK\r\n") && b.contains("Content-Length: 3000000\r\n") && b.ends_with("Connection: close\r\n\r\n"), "{b}");
        let c = String::from_utf8(c.join().unwrap()).unwrap();
        assert!(c.starts_with("HTTP/1.1 304 Not Modified\r\n") && c.ends_with("\r\n\r\n"), "{c}");
        assert!(String::from_utf8(d.join().unwrap()).unwrap().starts_with("HTTP/1.1 405 "));
        assert!(String::from_utf8(e.join().unwrap()).unwrap().starts_with("HTTP/1.1 404 "));
        assert_eq!(web.served, 5);
        assert_eq!(web.open(), 0);
        assert!(web.sent > 3_000_000);
    }

    #[test]
    fn silent_and_greedy_clients_are_let_go() {
        let mut web = Web::on(TcpListener::bind("127.0.0.1:0").unwrap()).unwrap();
        let port = web.port;
        // One address holds all the connections it may, each with half a request and then silence
        let held: Vec<TcpStream> = (0..MAX_PER_IP).map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap()).collect();
        for mut s in &held {
            s.write_all(b"GET / HTTP/1.1\r\nHost:").unwrap();
        }
        let now = Instant::now();
        let began = Instant::now();
        while web.open() < held.len() || web.conns.iter().any(|c| c.buf.is_empty()) {
            assert!(began.elapsed() < Duration::from_secs(5));
            web.poll(now, now, &mut |_| Response::not_found());
            std::thread::sleep(Duration::from_millis(1));
        }
        // None of them is idle, so three more from that address are turned away
        let extra: Vec<TcpStream> = (0..3).map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap()).collect();
        while web.refused < 3 {
            assert!(began.elapsed() < Duration::from_secs(5));
            web.poll(now, now, &mut |_| Response::not_found());
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!((web.open(), web.refused), (MAX_PER_IP, 3));
        drop(extra);
        // The silent ones are gone after the read timeout
        let later = now + READ_TIMEOUT + Duration::from_secs(1);
        web.poll(later, later, &mut |_| Response::not_found());
        assert_eq!((web.open(), web.served), (0, 0));
    }

    #[test]
    fn closed_connections_make_room_at_once() {
        let mut web = Web::on(TcpListener::bind("127.0.0.1:0").unwrap()).unwrap();
        let port = web.port;
        // One address, one request per connection, four times what it may hold at once
        for round in 0..4 {
            let clients: Vec<_> = (0..MAX_PER_IP).map(|_| client(port, b"GET /x HTTP/1.0\r\n\r\n")).collect();
            let began = Instant::now();
            while !clients.iter().all(|c| c.is_finished()) {
                assert!(began.elapsed() < Duration::from_secs(5), "round {round}");
                let now = Instant::now();
                web.poll(now, now + Duration::from_millis(2), &mut |_| Response::text(200, "ok"));
                std::thread::sleep(Duration::from_millis(1));
            }
            for c in clients {
                assert!(c.join().unwrap().ends_with(b"ok"));
            }
        }
        assert_eq!((web.served, web.refused), (4 * MAX_PER_IP as u64, 0));
    }

    #[test]
    fn a_full_house_makes_room_from_idle_connections() {
        let mut web = Web::on(TcpListener::bind("127.0.0.1:0").unwrap()).unwrap();
        let now = Instant::now();
        // Idle connections from many addresses fill the table (made by hand: loopback has one address)
        let port = web.port;
        let mut keep = Vec::new();
        for i in 0..MAX_CONNS {
            keep.push(TcpStream::connect(("127.0.0.1", port)).unwrap());
            let (stream, _) = loop {
                match web.listener.accept() {
                    Ok(pair) => break pair,
                    Err(_) => std::thread::sleep(Duration::from_millis(1)),
                }
            };
            stream.set_nonblocking(true).unwrap();
            let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, i as u8 + 1));
            web.conns.push(Conn { stream, ip, buf: Vec::new(), out: None, last: now + Duration::from_millis(i as u64), close: false, eof: false });
        }
        let newcomer = client(port, b"GET /x HTTP/1.0\r\n\r\n");
        let began = Instant::now();
        while !newcomer.is_finished() {
            assert!(began.elapsed() < Duration::from_secs(5));
            let t = now + Duration::from_secs(1);
            web.poll(t, t, &mut |_| Response::text(200, "ok"));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(newcomer.join().unwrap().ends_with(b"ok"));
        // The one idle the longest went; nobody was refused
        assert_eq!((web.open(), web.refused), (MAX_CONNS - 1, 0));
        assert!(!web.conns.iter().any(|c| c.ip == IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }
}
