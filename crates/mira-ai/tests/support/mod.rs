#![cfg(feature = "openai-completions")]

//! Offline loopback HTTP server for transport tests.
//!
//! The server is intentionally minimal: it reads one request per connection and answers from a
//! queue of scripted replies, so tests can control framing, delays, stalls and connection
//! closes without a live provider. Nothing here performs network I/O beyond `127.0.0.1`.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mira_ai::api::openai_completions::{
    OpenAiCompletionsCompat, OpenAiCompletionsConfig, OpenAiCompletionsProvider,
};
use mira_ai::{Api, Context, Credential, Model, StreamRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};

/// How a chunked reply ends.
pub enum ChunkedEnd {
    /// Terminate the chunked body normally.
    Terminate,
    /// Keep the connection open without sending more bytes.
    Stall,
    /// Close the connection in the middle of the body.
    Close,
}

/// How the server writes one scripted reply.
pub enum ReplyKind {
    /// Complete body with `Content-Length`.
    Complete(Vec<u8>),
    /// Chunked body, one entry per chunk with its own delay.
    Chunked {
        chunks: Vec<(Duration, Vec<u8>)>,
        end: ChunkedEnd,
    },
    /// Answer with an SSE body whose text is the request's bearer credential, so a test can
    /// pair a response with the exact request that produced it.
    EchoCredential,
    /// Read the request and never answer.
    Silent,
    /// Close the connection without answering.
    Close,
}

/// One scripted HTTP reply.
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub kind: ReplyKind,
}

impl Reply {
    fn new(status: u16, headers: Vec<(String, String)>, kind: ReplyKind) -> Self {
        Self {
            status,
            headers,
            kind,
        }
    }

    /// One complete SSE body sent with `Content-Length`.
    pub fn sse(body: &str) -> Self {
        Self::new(
            200,
            vec![("content-type".into(), "text/event-stream".into())],
            ReplyKind::Complete(body.as_bytes().to_vec()),
        )
    }

    /// Chunked SSE body, one frame per chunk, sent as fast as the socket accepts it.
    pub fn sse_frames(frames: &[&str]) -> Self {
        Self::sse_chunks(
            frames
                .iter()
                .map(|frame| (Duration::ZERO, frame.as_bytes().to_vec()))
                .collect(),
            ChunkedEnd::Terminate,
        )
    }

    /// Chunked SSE body with explicit per-chunk delays.
    pub fn sse_chunks(chunks: Vec<(Duration, Vec<u8>)>, end: ChunkedEnd) -> Self {
        Self::new(
            200,
            vec![("content-type".into(), "text/event-stream".into())],
            ReplyKind::Chunked { chunks, end },
        )
    }

    /// Chunked SSE body that stops after `frames` without terminating: the response stalls
    /// until the client gives up or cancels.
    pub fn sse_stalling(frames: &[&str]) -> Self {
        Self::sse_chunks(
            frames
                .iter()
                .map(|frame| (Duration::ZERO, frame.as_bytes().to_vec()))
                .collect(),
            ChunkedEnd::Stall,
        )
    }

    /// Chunked SSE body that is cut off after `frames`, as an interrupted connection would.
    pub fn sse_truncated(frames: &[&str]) -> Self {
        Self::sse_chunks(
            frames
                .iter()
                .map(|frame| (Duration::ZERO, frame.as_bytes().to_vec()))
                .collect(),
            ChunkedEnd::Close,
        )
    }

    /// SSE body whose text is the request credential, for pairing assertions.
    pub fn sse_echo_credential() -> Self {
        Self::new(200, Vec::new(), ReplyKind::EchoCredential)
    }

    /// JSON body, used for provider error responses.
    pub fn json(status: u16, body: serde_json::Value) -> Self {
        Self::new(
            status,
            vec![("content-type".into(), "application/json".into())],
            ReplyKind::Complete(body.to_string().into_bytes()),
        )
    }

    /// JSON body with additional headers.
    pub fn json_with_headers(
        status: u16,
        headers: Vec<(String, String)>,
        body: serde_json::Value,
    ) -> Self {
        let mut all = headers;
        all.push(("content-type".into(), "application/json".into()));
        Self::new(
            status,
            all,
            ReplyKind::Complete(body.to_string().into_bytes()),
        )
    }

    /// Plain-text body.
    pub fn text(status: u16, body: &str) -> Self {
        Self::new(
            status,
            vec![("content-type".into(), "text/plain".into())],
            ReplyKind::Complete(body.as_bytes().to_vec()),
        )
    }

    /// An error response whose headers arrive immediately and whose body never completes.
    pub fn stalled_body(status: u16) -> Self {
        Self::new(
            status,
            vec![("content-type".to_string(), "application/json".to_string())],
            ReplyKind::Chunked {
                chunks: Vec::new(),
                end: ChunkedEnd::Stall,
            },
        )
    }

    /// A redirect to another location, used to prove redirects are not followed.
    pub fn redirect(location: &str) -> Self {
        Self::new(
            302,
            vec![("location".to_string(), location.to_string())],
            ReplyKind::Complete(Vec::new()),
        )
    }

    /// The default reply when a test did not script one.
    pub fn server_error() -> Self {
        Self::json(
            500,
            serde_json::json!({"error": {"message": "no scripted reply"}}),
        )
    }

    /// Read the request and never answer.
    pub fn silent() -> Self {
        Self::new(200, Vec::new(), ReplyKind::Silent)
    }

    /// Close the connection without answering.
    pub fn closed() -> Self {
        Self::new(200, Vec::new(), ReplyKind::Close)
    }
}

/// One request the server received.
#[derive(Clone)]
pub struct RecordedRequest {
    pub head: String,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// Request line, for example `POST /v1/chat/completions HTTP/1.1`.
    pub fn request_line(&self) -> &str {
        self.head.lines().next().unwrap_or_default()
    }

    /// Request target, for example `/v1/chat/completions`.
    pub fn path(&self) -> &str {
        self.request_line()
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
    }

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().skip(1).find_map(|line| {
            let (field, value) = line.split_once(':')?;
            field
                .trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim())
        })
    }

    /// Parsed JSON request body.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
    }
}

struct ServerState {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<RecordedRequest>>,
    client_closures: AtomicUsize,
}

/// A loopback HTTP server for one test.
pub struct TestServer {
    address: SocketAddr,
    state: Arc<ServerState>,
}

impl TestServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local address");
        let state = Arc::new(ServerState {
            replies: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            client_closures: AtomicUsize::new(0),
        });
        let server_state = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let connection_state = server_state.clone();
                tokio::spawn(async move { handle_connection(stream, connection_state).await });
            }
        });
        Self { address, state }
    }

    /// Base URL to configure a provider with.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Queue the next reply.
    pub fn enqueue(&self, reply: Reply) {
        self.state.replies.lock().expect("replies").push_back(reply);
    }

    /// Snapshot of the requests received so far.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.lock().expect("requests").clone()
    }

    pub fn request_count(&self) -> usize {
        self.state.requests.lock().expect("requests").len()
    }

    /// Number of connections the client closed while the server was still holding them open.
    pub fn client_closures(&self) -> usize {
        self.state.client_closures.load(Ordering::SeqCst)
    }

    /// Wait until at least `count` requests arrived.
    pub async fn wait_for_requests(&self, count: usize) {
        timeout(Duration::from_secs(5), async {
            while self.request_count() < count {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("expected {count} requests, saw {}", self.request_count()));
    }

    /// Wait until the client closed a held-open connection.
    ///
    /// The assertion deadline is deliberately shorter than the server watchdog, so a passing
    /// test can only mean the socket really closed.
    pub async fn wait_for_client_closure(&self) {
        timeout(Duration::from_secs(1), async {
            while self.client_closures() == 0 {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the client never closed its connection"));
    }
}

async fn handle_connection(mut stream: TcpStream, state: Arc<ServerState>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let credential = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_string();
    state.requests.lock().expect("requests").push(request);
    let reply = state
        .replies
        .lock()
        .expect("replies")
        .pop_front()
        .unwrap_or_else(Reply::server_error);

    let Reply {
        status,
        headers,
        kind,
    } = reply;
    match kind {
        ReplyKind::Silent => observe_client_close(stream, &state).await,
        ReplyKind::Close => drop(stream),
        ReplyKind::EchoCredential => {
            let body = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                serde_json::json!({"choices": [{"delta": {"content": credential}}]}),
                serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            );
            let head = response_head(
                status,
                &[
                    ("content-length".to_string(), body.len().to_string()),
                    ("content-type".to_string(), "text/event-stream".to_string()),
                ],
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
            let _ = stream.flush().await;
        }
        ReplyKind::Complete(body) => {
            let mut all_headers = vec![("content-length".to_string(), body.len().to_string())];
            all_headers.extend(headers);
            let head = response_head(status, &all_headers);
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
            let _ = stream.flush().await;
        }
        ReplyKind::Chunked { chunks, end } => {
            let mut all_headers = vec![("transfer-encoding".to_string(), "chunked".to_string())];
            all_headers.extend(headers);
            let head = response_head(status, &all_headers);
            if stream.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            for (delay, chunk) in chunks {
                if !delay.is_zero() {
                    sleep(delay).await;
                }
                let frame = format!("{:x}\r\n", chunk.len());
                if stream.write_all(frame.as_bytes()).await.is_err()
                    || stream.write_all(&chunk).await.is_err()
                    || stream.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
                let _ = stream.flush().await;
            }
            match end {
                ChunkedEnd::Terminate => {
                    let _ = stream.write_all(b"0\r\n\r\n").await;
                    let _ = stream.flush().await;
                }
                ChunkedEnd::Stall => observe_client_close(stream, &state).await,
                ChunkedEnd::Close => drop(stream),
            }
        }
    }
}

/// Serialize one SSE `data:` payload.
pub fn json_frame(value: serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

/// Serialize a status line plus headers and the blank line that ends the head.
fn response_head(status: u16, headers: &[(String, String)]) -> String {
    format!(
        "HTTP/1.1 {} {}\r\n{}\r\n",
        status,
        reason(status),
        render_headers(headers),
    )
}

fn render_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// Read one HTTP request head and, when present, its `Content-Length` body.
async fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = timeout(Duration::from_secs(5), async {
        loop {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = find_head_end(&buffer) {
                return Some(position);
            }
        }
    })
    .await
    .ok()??;

    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut body = buffer[head_end..].to_vec();
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while body.len() < content_length {
        let read = timeout(Duration::from_secs(5), stream.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(RecordedRequest { head, body })
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

/// Wait for the client to close the connection, so tests can prove resource cleanup.
///
/// Only an observed EOF or socket error counts as a closure. The watchdog exists to keep handler
/// tasks from leaking; when it fires nothing is recorded, so the counter can never report a
/// closure that never happened.
async fn observe_client_close(mut stream: TcpStream, state: &ServerState) {
    const WATCHDOG: Duration = Duration::from_secs(5);
    let mut chunk = [0u8; 256];
    let observed = timeout(WATCHDOG, async {
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
    })
    .await;
    if observed.is_ok() {
        state.client_closures.fetch_add(1, Ordering::SeqCst);
    }
}

/// Credential used by tests. Long enough that redaction assertions are meaningful.
pub const TEST_CREDENTIAL: &str = "sk-test-credential-1234567890";

/// Streaming reply for a text response: content deltas, a finish reason and `[DONE]`.
pub fn text_reply(text: &str) -> Reply {
    Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": text}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n",
    ])
}

/// Provider pointed at one test server with default compatibility.
pub fn provider_for(server: &TestServer) -> OpenAiCompletionsProvider {
    provider_with(server, OpenAiCompletionsCompat::default())
}

/// Provider pointed at one test server with explicit compatibility.
pub fn provider_with(
    server: &TestServer,
    compat: OpenAiCompletionsCompat,
) -> OpenAiCompletionsProvider {
    let mut config = OpenAiCompletionsConfig::new(server.base_url()).expect("test base URL");
    config.compat = compat;
    OpenAiCompletionsProvider::new(config).expect("provider")
}

/// Config pointed at one test server.
pub fn config_for(server: &TestServer) -> OpenAiCompletionsConfig {
    OpenAiCompletionsConfig::new(server.base_url()).expect("test base URL")
}

/// A text-only model served by the Chat Completions transport.
pub fn text_model() -> Model {
    Model::new(Api::OpenAiCompletions, "test-provider", "test-model")
}

/// A request with the shared test credential.
pub fn request(context: Context) -> StreamRequest {
    StreamRequest::new(text_model(), context, Credential::new(TEST_CREDENTIAL))
}
