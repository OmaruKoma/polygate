//! Test-only deterministic HTTP server shared by provider and check-in tests.
//!
//! It speaks just enough HTTP/1.1 for `reqwest`: one request per connection,
//! `Content-Length`, `Connection: close`. Handlers see the exact method, target,
//! headers and body, so signed-header and body assertions are possible without
//! a network.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or(&self.target)
    }
}

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
    pub delay: Duration,
    /// Write this many body bytes, flush, then pause before sending the rest.
    pub stall: Option<(usize, Duration)>,
}

impl Reply {
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.into(),
            delay: Duration::ZERO,
            stall: None,
        }
    }

    pub fn sse(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            body: body.into(),
            delay: Duration::ZERO,
            stall: None,
        }
    }

    pub fn slow(delay: Duration) -> Self {
        Self {
            delay,
            ..Self::json(200, "{}")
        }
    }

    /// An SSE reply that sends `split_at` bytes, then stalls for `pause` before
    /// the remainder. Used to exercise mid-stream interruption handling.
    pub fn sse_stalled(body: impl Into<String>, split_at: usize, pause: Duration) -> Self {
        Self {
            stall: Some((split_at, pause)),
            ..Self::sse(body)
        }
    }
}

pub struct Server {
    pub base_url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().len()
    }
}

pub async fn spawn_server<F>(handler: F) -> Server
where
    F: Fn(&Recorded) -> Reply + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let handler = Arc::new(handler);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let recorded = Arc::clone(&recorded);
            let handler = Arc::clone(&handler);
            tokio::spawn(async move {
                let Some(request) = read_request(&mut socket).await else {
                    return;
                };
                let reply = handler(&request);
                recorded.lock().push(request);
                if !reply.delay.is_zero() {
                    tokio::time::sleep(reply.delay).await;
                }
                let reason = match reply.status {
                    200 => "OK",
                    400 => "Bad Request",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    404 => "Not Found",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    503 => "Service Unavailable",
                    _ => "Error",
                };
                let head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    reason,
                    reply.content_type,
                    reply.body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                if let Some((split_at, pause)) = reply.stall {
                    let split_at = split_at.min(reply.body.len());
                    let (first, rest) = reply.body.as_bytes().split_at(split_at);
                    let _ = socket.write_all(first).await;
                    let _ = socket.flush().await;
                    tokio::time::sleep(pause).await;
                    let _ = socket.write_all(rest).await;
                } else {
                    let _ = socket.write_all(reply.body.as_bytes()).await;
                }
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            });
        }
    });
    Server {
        base_url: format!("http://{addr}"),
        requests,
        task,
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<Recorded> {
    let mut buffer = Vec::new();
    let header_end = loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position;
        }
        let mut chunk = [0u8; 4096];
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        if key.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        }
        headers.push((key.trim().to_string(), value));
    }
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0u8; content_length - body.len()];
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Some(Recorded {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
