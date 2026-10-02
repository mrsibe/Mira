//! Phase-1 Pi AI sidecar seam.
//!
//! Each inference request owns a single child process. The request, including
//! the provider API key, travels only through the child's stdin as one JSONL
//! `complete` frame; the child answers on stdout with JSONL `text_delta` /
//! `thinking_delta` frames and exactly one terminal `done` or `error` frame.
//! Credentials never reach argv, environment variables or logs, and child
//! stderr is discarded. There is no HTTP fallback path here.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::{timeout, Instant};

const PROTOCOL_VERSION: u32 = 1;
const RUNTIME_BINARY_NAME: &str = "mira-runtime";
const READ_CHUNK_BYTES: usize = 8 * 1024;
// Shared protocol limits: a request line may be up to 8 MiB, a stdout event
// line up to 1 MiB.
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
// Inactivity bound is wider than the sidecar's provider retry budget
// (three 45s attempts plus two <=5s Retry-After waits) so a retrying
// provider is not cut off.
const READ_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_SESSION_DURATION: Duration = Duration::from_secs(300);
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(25);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// Provider metadata forwarded to the sidecar. `api_key` is only ever
/// serialized into the stdin request line.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeConfig {
    pub provider: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeMessage {
    pub role: String,
    pub content: String,
}

pub struct StreamRequest {
    pub config: RuntimeConfig,
    pub messages: Vec<RuntimeMessage>,
    pub temperature: Option<f32>,
}

#[derive(Debug)]
pub enum RuntimeError {
    Cancelled,
    Provider(String),
    Protocol(String),
    Spawn(String),
    Io(String),
    Callback(String),
    Timeout,
}

impl RuntimeError {
    /// User-facing message. Never includes raw frame payloads.
    pub fn message(&self) -> String {
        match self {
            RuntimeError::Cancelled => "模型请求已取消".to_string(),
            RuntimeError::Provider(message) => format!("模型请求失败: {message}"),
            RuntimeError::Protocol(reason) => format!("模型运行时协议错误: {reason}"),
            RuntimeError::Spawn(reason) => format!("模型运行时启动失败: {reason}"),
            RuntimeError::Io(reason) => format!("模型运行时通信失败: {reason}"),
            RuntimeError::Callback(message) => message.clone(),
            RuntimeError::Timeout => "模型请求超时".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SessionLimits {
    inactivity: Duration,
    max_duration: Duration,
    cancel_poll: Duration,
    write: Duration,
    reap: Duration,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            inactivity: READ_INACTIVITY_TIMEOUT,
            max_duration: MAX_SESSION_DURATION,
            cancel_poll: CANCEL_POLL_INTERVAL,
            write: WRITE_TIMEOUT,
            reap: CHILD_REAP_TIMEOUT,
        }
    }
}

#[derive(Serialize)]
struct RequestEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    frame_type: &'static str,
    id: &'a str,
    config: &'a RuntimeConfig,
    messages: &'a [RuntimeMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Deserialize)]
struct ChildFrame {
    version: u32,
    id: String,
    #[serde(rename = "type")]
    frame_type: String,
    delta: Option<String>,
    content: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

/// Runs one completion through its own sidecar child process. Every call spawns
/// an independent process, so a background memory pass and a chat stream never
/// share state.
pub async fn complete<F, G>(
    request: StreamRequest,
    on_delta: &mut F,
    on_thinking: &mut G,
    cancel_requested: &AtomicBool,
) -> Result<String, RuntimeError>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    let program = resolve_runtime_binary()?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_line = build_request_envelope(&request_id, &request)?;
    run_child(
        Command::new(&program),
        &request_line,
        &request_id,
        on_delta,
        on_thinking,
        cancel_requested,
        SessionLimits::default(),
    )
    .await
}

fn build_request_envelope(id: &str, request: &StreamRequest) -> Result<String, RuntimeError> {
    let line = serde_json::to_string(&RequestEnvelope {
        version: PROTOCOL_VERSION,
        frame_type: "complete",
        id,
        config: &request.config,
        messages: &request.messages,
        temperature: request.temperature,
    })
    .map_err(|error| RuntimeError::Protocol(format!("无法编码请求帧: {error}")))?;
    if line.len() + 1 > MAX_REQUEST_BYTES {
        return Err(RuntimeError::Protocol(format!(
            "请求帧超过 {} MiB 上限",
            MAX_REQUEST_BYTES / (1024 * 1024)
        )));
    }
    Ok(line)
}

/// Absolute path of the sidecar without checking existence. A relative path is
/// never produced, so `Command` cannot fall back to searching `PATH`.
fn runtime_binary_path() -> Result<PathBuf, RuntimeError> {
    if cfg!(debug_assertions) {
        // Dev builds read the staged sidecar next to the sources, where the
        // Tauri CLI expects `<name>-<target triple>`.
        let triple = env!("MIRA_RUNTIME_TARGET_TRIPLE");
        let file = if cfg!(windows) {
            format!("{RUNTIME_BINARY_NAME}-{triple}.exe")
        } else {
            format!("{RUNTIME_BINARY_NAME}-{triple}")
        };
        Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(file))
    } else {
        // Bundlers place external binaries as a sibling of the main
        // executable (Linux deb/AppImage `usr/bin`, macOS `Contents/MacOS`,
        // Windows install dir), with the target triple suffix stripped.
        let exe = std::env::current_exe()
            .map_err(|error| RuntimeError::Spawn(format!("无法定位当前可执行文件: {error}")))?;
        let dir = exe
            .parent()
            .ok_or_else(|| RuntimeError::Spawn("当前可执行文件没有父目录".to_string()))?;
        let file = if cfg!(windows) {
            format!("{RUNTIME_BINARY_NAME}.exe")
        } else {
            RUNTIME_BINARY_NAME.to_string()
        };
        Ok(dir.join(file))
    }
}

fn resolve_runtime_binary() -> Result<PathBuf, RuntimeError> {
    let path = runtime_binary_path()?;
    if !path.is_file() {
        return Err(RuntimeError::Spawn(format!(
            "未找到 mira-runtime sidecar: {}",
            path.display()
        )));
    }
    Ok(path)
}

async fn run_child<F, G>(
    mut command: Command,
    request_line: &str,
    request_id: &str,
    on_delta: &mut F,
    on_thinking: &mut G,
    cancel_requested: &AtomicBool,
    limits: SessionLimits,
) -> Result<String, RuntimeError>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("OPENAI_LOG", "off")
        .kill_on_drop(true);

    if cancel_requested.load(Ordering::SeqCst) {
        return Err(RuntimeError::Cancelled);
    }
    let mut child = command
        .spawn()
        .map_err(|error| RuntimeError::Spawn(format!("无法启动 sidecar: {error}")))?;
    if cancel_requested.load(Ordering::SeqCst) {
        terminate_child(&mut child, limits.reap).await;
        return Err(RuntimeError::Cancelled);
    }

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| RuntimeError::Spawn("sidecar stdin 不可用".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RuntimeError::Spawn("sidecar stdout 不可用".to_string()))?;

    // The request line is large histories plus the credential, so write it
    // concurrently with reading stdout instead of risking a full pipe.
    let outcome = {
        let writer = write_request_line(stdin, request_line, limits.write);
        let session = run_session(
            stdout,
            request_id,
            on_delta,
            on_thinking,
            cancel_requested,
            limits,
        );
        tokio::pin!(writer, session);
        tokio::select! {
            result = &mut session => result,
            written = &mut writer => match written {
                Ok(()) => session.await,
                Err(error) => Err(error),
            },
        }
        // Dropping the writer closes stdin on every outcome, including cancel.
    };

    match outcome {
        Ok(content) => {
            finish_child(&mut child, limits.reap).await;
            Ok(content)
        }
        Err(error) => {
            terminate_child(&mut child, limits.reap).await;
            Err(error)
        }
    }
}

async fn write_request_line(
    mut stdin: ChildStdin,
    request_line: &str,
    write_timeout: Duration,
) -> Result<(), RuntimeError> {
    let write = async {
        stdin.write_all(request_line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.shutdown().await
    };
    timeout(write_timeout, write)
        .await
        .map_err(|_| RuntimeError::Timeout)?
        .map_err(|_| RuntimeError::Io("写入 sidecar 请求失败".to_string()))
}

async fn run_session<R, F, G>(
    mut reader: R,
    request_id: &str,
    on_delta: &mut F,
    on_thinking: &mut G,
    cancel_requested: &AtomicBool,
    limits: SessionLimits,
) -> Result<String, RuntimeError>
where
    R: AsyncRead + Unpin,
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    let mut pending: Vec<u8> = Vec::new();
    let session_deadline = Instant::now() + limits.max_duration;
    let mut inactivity_deadline = Instant::now() + limits.inactivity;
    let mut cancel_interval = tokio::time::interval(limits.cancel_poll);
    cancel_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        if cancel_requested.load(Ordering::SeqCst) {
            return Err(RuntimeError::Cancelled);
        }

        let read_result = tokio::select! {
            _ = cancel_interval.tick() => {
                if cancel_requested.load(Ordering::SeqCst) {
                    return Err(RuntimeError::Cancelled);
                }
                // Ticking must not extend the inactivity window.
                continue;
            }
            _ = tokio::time::sleep_until(inactivity_deadline) => {
                return Err(RuntimeError::Timeout);
            }
            _ = tokio::time::sleep_until(session_deadline) => {
                return Err(RuntimeError::Timeout);
            }
            result = reader.read(&mut chunk) => result,
        };

        match read_result {
            Ok(0) => {
                if !pending.is_empty() {
                    let line = std::mem::take(&mut pending);
                    if let Some(content) =
                        handle_frame_line(&line, request_id, on_delta, on_thinking)?
                    {
                        return Ok(content);
                    }
                }
                return Err(RuntimeError::Protocol(
                    "sidecar 在终止帧之前关闭了 stdout".to_string(),
                ));
            }
            Ok(read_len) => {
                pending.extend_from_slice(&chunk[..read_len]);
                inactivity_deadline = Instant::now() + limits.inactivity;
                while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                    let mut line: Vec<u8> = pending.drain(..=newline).collect();
                    line.pop();
                    if let Some(content) =
                        handle_frame_line(&line, request_id, on_delta, on_thinking)?
                    {
                        return Ok(content);
                    }
                }
                if pending.len() > MAX_FRAME_BYTES {
                    return Err(RuntimeError::Protocol("sidecar 帧超过大小上限".to_string()));
                }
            }
            Err(error) => {
                return Err(RuntimeError::Io(format!("读取 sidecar 输出失败: {error}")));
            }
        }
    }
}

/// Parses one frame line. Returns `Ok(Some(content))` for a terminal `done`
/// frame, `Ok(None)` for a delta, and `Err` for any fatal condition. Rejection
/// messages describe the shape only and never echo the payload.
fn handle_frame_line<F, G>(
    line: &[u8],
    request_id: &str,
    on_delta: &mut F,
    on_thinking: &mut G,
) -> Result<Option<String>, RuntimeError>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    if line.len() + 1 > MAX_FRAME_BYTES {
        return Err(RuntimeError::Protocol("sidecar 帧超过大小上限".to_string()));
    }
    let trimmed = line.trim_ascii();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let frame: ChildFrame = serde_json::from_slice(trimmed)
        .map_err(|_| RuntimeError::Protocol("sidecar 输出了格式错误的帧".to_string()))?;
    if frame.version != PROTOCOL_VERSION {
        return Err(RuntimeError::Protocol(
            "sidecar 帧协议版本不匹配".to_string(),
        ));
    }
    if frame.id != request_id {
        return Err(RuntimeError::Protocol(
            "sidecar 帧请求 id 不匹配".to_string(),
        ));
    }

    match frame.frame_type.as_str() {
        "text_delta" => {
            let delta = frame
                .delta
                .as_deref()
                .ok_or_else(|| RuntimeError::Protocol("text_delta 帧缺少 delta".to_string()))?;
            on_delta(delta).map_err(RuntimeError::Callback)?;
            Ok(None)
        }
        "thinking_delta" => {
            let delta = frame
                .delta
                .as_deref()
                .ok_or_else(|| RuntimeError::Protocol("thinking_delta 帧缺少 delta".to_string()))?;
            on_thinking(delta).map_err(RuntimeError::Callback)?;
            Ok(None)
        }
        "done" => {
            let content = frame
                .content
                .ok_or_else(|| RuntimeError::Protocol("done 帧缺少 content".to_string()))?;
            Ok(Some(content))
        }
        "error" => {
            let code = frame
                .code
                .as_deref()
                .ok_or_else(|| RuntimeError::Protocol("error 帧缺少 code".to_string()))?;
            let message = frame
                .message
                .ok_or_else(|| RuntimeError::Protocol("error 帧缺少 message".to_string()))?;
            match code {
                "cancelled" => Err(RuntimeError::Cancelled),
                "provider" => Err(RuntimeError::Provider(message)),
                "protocol" => Err(RuntimeError::Protocol("sidecar 报告了协议错误".to_string())),
                _ => Err(RuntimeError::Protocol(
                    "sidecar 报告了未知错误码".to_string(),
                )),
            }
        }
        _ => Err(RuntimeError::Protocol(
            "sidecar 输出了未知帧类型".to_string(),
        )),
    }
}

async fn finish_child(child: &mut Child, reap_timeout: Duration) {
    if timeout(reap_timeout, child.wait()).await.is_err() {
        terminate_child(child, reap_timeout).await;
    }
}

async fn terminate_child(child: &mut Child, reap_timeout: Duration) {
    let _ = child.start_kill();
    let _ = timeout(reap_timeout, child.wait()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn request(api_key: &str, temperature: Option<f32>) -> StreamRequest {
        StreamRequest {
            config: RuntimeConfig {
                provider: "openai".to_string(),
                name: "Mock provider".to_string(),
                base_url: "https://api.example.com/v1".to_string(),
                model: "gpt-4o-mini".to_string(),
                api_key: api_key.to_string(),
            },
            messages: vec![
                RuntimeMessage {
                    role: "system".to_string(),
                    content: "be brief".to_string(),
                },
                RuntimeMessage {
                    role: "user".to_string(),
                    content: "hi".to_string(),
                },
            ],
            temperature,
        }
    }

    fn limits(inactivity: Duration) -> SessionLimits {
        SessionLimits {
            inactivity,
            max_duration: Duration::from_secs(5),
            cancel_poll: Duration::from_millis(10),
            write: Duration::from_secs(5),
            reap: Duration::from_secs(5),
        }
    }

    async fn run_frames(
        frames: &str,
        request_id: &str,
    ) -> (Result<String, RuntimeError>, Vec<String>, Vec<String>) {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        writer
            .write_all(frames.as_bytes())
            .await
            .expect("frames should buffer");
        drop(writer);

        let mut deltas = Vec::new();
        let mut thinking = Vec::new();
        let cancel = AtomicBool::new(false);
        let result = {
            let mut on_delta = |value: &str| -> Result<(), String> {
                deltas.push(value.to_string());
                Ok(())
            };
            let mut on_thinking = |value: &str| -> Result<(), String> {
                thinking.push(value.to_string());
                Ok(())
            };
            run_session(
                reader,
                request_id,
                &mut on_delta,
                &mut on_thinking,
                &cancel,
                limits(Duration::from_millis(200)),
            )
            .await
        };
        (result, deltas, thinking)
    }

    #[tokio::test]
    async fn streams_deltas_and_returns_terminal_content() {
        let frames = concat!(
            "{\"version\":1,\"id\":\"req-1\",\"type\":\"thinking_delta\",\"delta\":\"think\"}\n",
            "{\"version\":1,\"id\":\"req-1\",\"type\":\"text_delta\",\"delta\":\"Hel\"}\n",
            "{\"version\":1,\"id\":\"req-1\",\"type\":\"text_delta\",\"delta\":\"lo\"}\n",
            "{\"version\":1,\"id\":\"req-1\",\"type\":\"done\",\"content\":\"Hello\"}\n",
        );

        let (result, deltas, thinking) = run_frames(frames, "req-1").await;

        assert_eq!(result.expect("session should finish"), "Hello");
        assert_eq!(deltas, vec!["Hel".to_string(), "lo".to_string()]);
        assert_eq!(thinking, vec!["think".to_string()]);
    }

    #[tokio::test]
    async fn parses_a_terminal_frame_without_a_trailing_newline() {
        let frames = "{\"version\":1,\"id\":\"req-1\",\"type\":\"done\",\"content\":\"ok\"}";

        let (result, _, _) = run_frames(frames, "req-1").await;

        assert_eq!(result.expect("trailing frame should parse"), "ok");
    }

    #[tokio::test]
    async fn error_frame_maps_cancelled_code() {
        let frames =
            "{\"version\":1,\"id\":\"req-1\",\"type\":\"error\",\"code\":\"cancelled\",\"message\":\"stop\"}\n";

        let (result, _, _) = run_frames(frames, "req-1").await;

        assert!(matches!(result, Err(RuntimeError::Cancelled)));
    }

    #[tokio::test]
    async fn provider_error_frame_surfaces_the_provider_message() {
        let frames =
            "{\"version\":1,\"id\":\"req-1\",\"type\":\"error\",\"code\":\"provider\",\"message\":\"upstream 429\"}\n";

        let (result, _, _) = run_frames(frames, "req-1").await;

        match result {
            Err(RuntimeError::Provider(message)) => assert_eq!(message, "upstream 429"),
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mismatched_request_id_is_a_protocol_error() {
        let frames = "{\"version\":1,\"id\":\"other\",\"type\":\"done\",\"content\":\"x\"}\n";

        let (result, _, _) = run_frames(frames, "req-1").await;

        match result {
            Err(RuntimeError::Protocol(reason)) => assert!(reason.contains("id")),
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mismatched_protocol_version_is_a_protocol_error() {
        let frames = "{\"version\":2,\"id\":\"req-1\",\"type\":\"done\",\"content\":\"x\"}\n";

        let (result, _, _) = run_frames(frames, "req-1").await;

        match result {
            Err(RuntimeError::Protocol(reason)) => assert!(reason.contains("版本")),
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_frame_type_is_a_protocol_error() {
        let frames = "{\"version\":1,\"id\":\"req-1\",\"type\":\"mystery\"}\n";

        let (result, _, _) = run_frames(frames, "req-1").await;

        assert!(matches!(result, Err(RuntimeError::Protocol(_))));
    }

    #[tokio::test]
    async fn malformed_frame_does_not_echo_its_payload() {
        let frames = "SENTINEL-DO-NOT-ECHO {not json}\n";

        let (result, _, _) = run_frames(frames, "req-1").await;

        match result {
            Err(RuntimeError::Protocol(reason)) => assert!(!reason.contains("SENTINEL")),
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn eof_without_terminal_frame_is_a_protocol_error() {
        let frames =
            "{\"version\":1,\"id\":\"req-1\",\"type\":\"text_delta\",\"delta\":\"partial\"}\n";

        let (result, _, _) = run_frames(frames, "req-1").await;

        assert!(matches!(result, Err(RuntimeError::Protocol(_))));
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected() {
        let (mut writer, reader) = tokio::io::duplex(MAX_FRAME_BYTES * 2);
        let payload = vec![b'a'; MAX_FRAME_BYTES + 32];
        writer
            .write_all(&payload)
            .await
            .expect("payload should buffer");
        drop(writer);

        let cancel = AtomicBool::new(false);
        let mut on_delta = |_: &str| -> Result<(), String> { Ok(()) };
        let mut on_thinking = |_: &str| -> Result<(), String> { Ok(()) };
        let result = run_session(
            reader,
            "req-1",
            &mut on_delta,
            &mut on_thinking,
            &cancel,
            limits(Duration::from_millis(500)),
        )
        .await;

        match result {
            Err(RuntimeError::Protocol(reason)) => assert!(reason.contains("大小")),
            other => panic!("expected size protocol error, got {other:?}"),
        }
    }

    #[test]
    fn oversized_complete_frame_is_rejected_before_callbacks() {
        let line = format!(
            "{{\"version\":1,\"id\":\"req-1\",\"type\":\"done\",\"content\":\"{}\"}}",
            "x".repeat(MAX_FRAME_BYTES)
        );
        let mut callback = |_: &str| -> Result<(), String> { panic!("oversized frame delivered") };
        let mut thinking = |_: &str| -> Result<(), String> { Ok(()) };
        assert!(matches!(
            handle_frame_line(line.as_bytes(), "req-1", &mut callback, &mut thinking),
            Err(RuntimeError::Protocol(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires pnpm runtime:build; offline compiled-sidecar integration"]
    async fn compiled_runtime_streams_through_rust_bridge() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&chunk[..read]);
                if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..index]);
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|value| value.parse().ok())
                        })
                        .unwrap();
                    if bytes.len() >= index + 4 + length {
                        let request: serde_json::Value =
                            serde_json::from_slice(&bytes[index + 4..]).unwrap();
                        assert_eq!(request["messages"][0]["content"], "be brief");
                        assert!(request.get("tools").is_none());
                        break;
                    }
                }
            }
            let body = concat!(
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"思考\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你好 🌍\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let mut request = request("fixture-key", None);
        request.config.base_url = format!("http://{address}/v1");
        let mut deltas = String::new();
        let mut thoughts = String::new();
        let content = complete(
            request,
            &mut |delta| {
                deltas.push_str(delta);
                Ok(())
            },
            &mut |delta| {
                thoughts.push_str(delta);
                Ok(())
            },
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(content, "你好 🌍");
        assert_eq!(deltas, content);
        assert_eq!(thoughts, "思考");
    }

    #[tokio::test]
    async fn inactivity_timeout_stops_a_silent_stream() {
        let (_writer, reader) = tokio::io::duplex(1024);
        let cancel = AtomicBool::new(false);
        let mut on_delta = |_: &str| -> Result<(), String> { Ok(()) };
        let mut on_thinking = |_: &str| -> Result<(), String> { Ok(()) };

        let result = run_session(
            reader,
            "req-1",
            &mut on_delta,
            &mut on_thinking,
            &cancel,
            limits(Duration::from_millis(80)),
        )
        .await;

        assert!(matches!(result, Err(RuntimeError::Timeout)));
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_silent_stream() {
        let (writer, reader) = tokio::io::duplex(1024);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_handle = Arc::clone(&cancel);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_handle.store(true, Ordering::SeqCst);
        });
        let mut on_delta = |_: &str| -> Result<(), String> { Ok(()) };
        let mut on_thinking = |_: &str| -> Result<(), String> { Ok(()) };

        let started = std::time::Instant::now();
        let result = run_session(
            reader,
            "req-1",
            &mut on_delta,
            &mut on_thinking,
            &cancel,
            limits(Duration::from_secs(3)),
        )
        .await;
        drop(writer);

        assert!(matches!(result, Err(RuntimeError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn request_envelope_places_credentials_in_the_stdin_payload() {
        let line = build_request_envelope("req-1", &request("test-secret-key", Some(0.1)))
            .expect("request should encode");
        let value: serde_json::Value = serde_json::from_str(&line).expect("request should be JSON");

        assert_eq!(value["version"], 1);
        assert_eq!(value["type"], "complete");
        assert_eq!(value["id"], "req-1");
        assert_eq!(value["config"]["api_key"], "test-secret-key");
        assert_eq!(value["messages"][0]["role"], "system");
        let temperature = value["temperature"]
            .as_f64()
            .expect("temperature should be a number");
        assert!((temperature - 0.1).abs() < 1e-6);
    }

    #[test]
    fn request_envelope_omits_absent_temperature() {
        let line = build_request_envelope("req-1", &request("test-secret-key", None))
            .expect("request should encode");
        let value: serde_json::Value = serde_json::from_str(&line).expect("request should be JSON");

        assert!(value.get("temperature").is_none());
    }

    #[test]
    fn oversized_request_is_rejected_before_spawn() {
        let mut oversized = request("test-secret-key", None);
        oversized.messages = vec![RuntimeMessage {
            role: "user".to_string(),
            content: "x".repeat(MAX_REQUEST_BYTES),
        }];

        match build_request_envelope("req-1", &oversized) {
            Err(RuntimeError::Protocol(reason)) => assert!(reason.contains("8 MiB")),
            other => panic!("expected request size error, got {other:?}"),
        }
    }

    #[test]
    fn runtime_binary_path_is_absolute_and_target_suffixed() {
        let path = runtime_binary_path().expect("path should resolve");
        assert!(path.is_absolute());

        if cfg!(debug_assertions) {
            assert_eq!(
                path.parent()
                    .and_then(|parent| parent.file_name())
                    .and_then(|name| name.to_str()),
                Some("binaries")
            );
            let triple = env!("MIRA_RUNTIME_TARGET_TRIPLE");
            let expected = if cfg!(windows) {
                format!("{RUNTIME_BINARY_NAME}-{triple}.exe")
            } else {
                format!("{RUNTIME_BINARY_NAME}-{triple}")
            };
            assert_eq!(
                path.file_name().and_then(|name| name.to_str()),
                Some(expected.as_str())
            );
        } else {
            let expected = if cfg!(windows) {
                format!("{RUNTIME_BINARY_NAME}.exe")
            } else {
                RUNTIME_BINARY_NAME.to_string()
            };
            assert_eq!(
                path.file_name().and_then(|name| name.to_str()),
                Some(expected.as_str())
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_terminates_a_running_child_process() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("exec sleep 30");

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_handle = Arc::clone(&cancel);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            cancel_handle.store(true, Ordering::SeqCst);
        });

        let request_line = build_request_envelope("req-cancel", &request("SK-TEST-MARKER", None))
            .expect("request should encode");
        let mut deltas = Vec::new();
        let started = std::time::Instant::now();
        let result = {
            let mut on_delta = |value: &str| -> Result<(), String> {
                deltas.push(value.to_string());
                Ok(())
            };
            let mut on_thinking = |_: &str| -> Result<(), String> { Ok(()) };
            run_child(
                command,
                &request_line,
                "req-cancel",
                &mut on_delta,
                &mut on_thinking,
                &cancel,
                limits(Duration::from_secs(3)),
            )
            .await
        };

        assert!(matches!(result, Err(RuntimeError::Cancelled)));
        assert!(deltas.is_empty());
        // A reaped child returns well before the 30s sleep would finish.
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mock_child_receives_credentials_and_streams_frames() {
        use std::os::unix::fs::PermissionsExt;

        let script_path =
            std::env::temp_dir().join(format!("mira-runtime-mock-{}.sh", uuid::Uuid::new_v4()));
        let script = r#"#!/bin/sh
read -r request || exit 1
case "$request" in
  *SK-TEST-MARKER*) marker=present ;;
  *) marker=absent ;;
esac
printf '%s\n' "{\"version\":1,\"id\":\"req-script\",\"type\":\"text_delta\",\"delta\":\"scrip\"}"
printf '%s\n' "{\"version\":1,\"id\":\"req-script\",\"type\":\"done\",\"content\":\"$marker\"}"
"#;
        std::fs::write(&script_path, script).expect("script should write");
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("script should be executable");

        let request_line =
            build_request_envelope("req-script", &request("SK-TEST-MARKER", Some(0.1)))
                .expect("request should encode");
        let cancel = AtomicBool::new(false);
        let mut deltas = Vec::new();
        let result = {
            let mut on_delta = |value: &str| -> Result<(), String> {
                deltas.push(value.to_string());
                Ok(())
            };
            let mut on_thinking = |_: &str| -> Result<(), String> { Ok(()) };
            run_child(
                Command::new(&script_path),
                &request_line,
                "req-script",
                &mut on_delta,
                &mut on_thinking,
                &cancel,
                limits(Duration::from_secs(3)),
            )
            .await
        };
        let _ = std::fs::remove_file(&script_path);

        assert_eq!(result.expect("child should complete"), "present");
        assert_eq!(deltas, vec!["scrip".to_string()]);
    }
}
