//! Bounded server-sent-events framing.
//!
//! The framer accepts arbitrary byte fragmentation: a multi-byte character or a line
//! terminator may be split across any number of network chunks. Only complete lines are
//! decoded as UTF-8. Frames are dispatched on a blank line, `data` fields are joined with a
//! newline, comments and unknown fields are ignored and empty events are skipped.

use crate::error::AiError;

/// Default maximum size of one SSE event payload.
pub(crate) const DEFAULT_MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;

/// Incremental SSE parser for one response body.
pub(crate) struct SseParser {
    buffer: Vec<u8>,
    data: String,
    has_data_line: bool,
    max_event_bytes: usize,
    finished: bool,
}

impl SseParser {
    pub(crate) fn new(max_event_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            data: String::new(),
            has_data_line: false,
            max_event_bytes,
            finished: false,
        }
    }

    /// Feed response bytes and return every complete event payload they produced.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, AiError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.buffer.extend_from_slice(bytes);
        self.drain_lines()
    }

    /// Flush the end of the body: a line without its terminator and a pending event both
    /// count as complete at EOF. A stream that lacks a provider finish reason is still
    /// rejected by the caller, so this never turns truncation into success.
    pub(crate) fn finish(&mut self) -> Result<Vec<String>, AiError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        let mut frames = Vec::new();
        if !self.buffer.is_empty() {
            let mut line = std::mem::take(&mut self.buffer);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.handle_line(&line, &mut frames)?;
        }
        self.dispatch(&mut frames);
        Ok(frames)
    }

    fn drain_lines(&mut self) -> Result<Vec<String>, AiError> {
        let mut frames = Vec::new();
        while let Some((end, next)) = line_end(&self.buffer) {
            let line = self.buffer[..end].to_vec();
            self.buffer.drain(..next);
            self.handle_line(&line, &mut frames)?;
        }
        if self.buffer.len() > self.max_event_bytes {
            return Err(self.too_large());
        }
        Ok(frames)
    }

    fn handle_line(&mut self, line: &[u8], frames: &mut Vec<String>) -> Result<(), AiError> {
        let line = std::str::from_utf8(line).map_err(|_| {
            AiError::Protocol("provider stream contained a non-UTF-8 event line".to_string())
        })?;
        if line.is_empty() {
            self.dispatch(frames);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        if field == "data" {
            if !self.has_data_line {
                self.data.clear();
                self.has_data_line = true;
            }
            self.data.push_str(value);
            self.data.push('\n');
            if self.data.len() > self.max_event_bytes {
                return Err(self.too_large());
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, frames: &mut Vec<String>) {
        if !self.has_data_line {
            return;
        }
        let payload = self
            .data
            .strip_suffix('\n')
            .unwrap_or(&self.data)
            .to_string();
        self.data.clear();
        self.has_data_line = false;
        if payload.trim().is_empty() {
            return;
        }
        frames.push(payload);
    }

    fn too_large(&self) -> AiError {
        AiError::Protocol(format!(
            "provider stream event exceeded {} bytes",
            self.max_event_bytes
        ))
    }
}

/// Find the end of the first line, tolerating `\n`, `\r\n` and a lone `\r`.
///
/// A trailing `\r` is never treated as a line ending on its own: it may be the first half of
/// a `\r\n` that arrives in the next network chunk.
fn line_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0;
    while index < buffer.len() {
        match buffer[index] {
            b'\n' => return Some((index, index + 1)),
            b'\r' => {
                if index + 1 == buffer.len() {
                    return None;
                }
                let next = if buffer[index + 1] == b'\n' {
                    index + 2
                } else {
                    index + 1
                };
                return Some((index, next));
            }
            _ => index += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(parser: &mut SseParser, chunks: &[&[u8]]) -> Vec<String> {
        let mut frames = Vec::new();
        for chunk in chunks {
            frames.extend(parser.push(chunk).expect("push"));
        }
        frames.extend(parser.finish().expect("finish"));
        frames
    }

    #[test]
    fn splits_lf_and_crlf_frames() {
        let payloads = feed(
            &mut SseParser::new(DEFAULT_MAX_SSE_EVENT_BYTES),
            &[b"data: one\n\ndata: two\r\n\r\n"],
        );
        assert_eq!(payloads, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn reassembles_lines_split_across_chunks() {
        let payloads = feed(
            &mut SseParser::new(DEFAULT_MAX_SSE_EVENT_BYTES),
            &[b"da", b"ta: {\"a\":", b"1}\n", b"\n"],
        );
        assert_eq!(payloads, vec!["{\"a\":1}".to_string()]);
    }

    #[test]
    fn waits_for_the_second_half_of_a_split_crlf() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_EVENT_BYTES);
        assert!(parser.push(b"data: one\r").expect("push").is_empty());
        assert_eq!(
            parser.push(b"\n\r\n").expect("push"),
            vec!["one".to_string()]
        );
    }

    #[test]
    fn joins_multiline_data_and_ignores_comments_and_unknown_fields() {
        let payloads = feed(
            &mut SseParser::new(DEFAULT_MAX_SSE_EVENT_BYTES),
            &[b": keep-alive\nevent: message\nid: 7\nretry: 100\ndata: {\"a\":\ndata: 1\n\n\n\ndata:\n\n"],
        );
        assert_eq!(payloads, vec!["{\"a\":\n1".to_string()]);
    }

    #[test]
    fn accepts_data_without_a_space() {
        let payloads = feed(
            &mut SseParser::new(DEFAULT_MAX_SSE_EVENT_BYTES),
            &[b"data:{\"a\":1}\n\n"],
        );
        assert_eq!(payloads, vec!["{\"a\":1}".to_string()]);
    }

    #[test]
    fn flushes_a_final_event_without_a_blank_line() {
        let payloads = feed(
            &mut SseParser::new(DEFAULT_MAX_SSE_EVENT_BYTES),
            &[b"data: {\"a\":1}"],
        );
        assert_eq!(payloads, vec!["{\"a\":1}".to_string()]);
    }

    #[test]
    fn fails_closed_on_an_oversized_event() {
        let mut parser = SseParser::new(16);
        let error = parser
            .push(b"data: 0123456789012345678901234567890\n\n")
            .expect_err("oversized");
        assert!(matches!(error, AiError::Protocol(_)));
    }

    #[test]
    fn fails_closed_on_an_oversized_unterminated_line() {
        let mut parser = SseParser::new(8);
        let error = parser.push(b"data: 0123456789").expect_err("oversized");
        assert!(matches!(error, AiError::Protocol(_)));
    }

    #[test]
    fn fails_closed_on_invalid_utf8() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_EVENT_BYTES);
        let error = parser
            .push(b"data: \xff\xfe\n\n")
            .expect_err("invalid utf8");
        assert!(matches!(error, AiError::Protocol(_)));
    }
}
