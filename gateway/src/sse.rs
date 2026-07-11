use std::fmt;

use serde_json::Value;

pub const DEFAULT_MAX_SSE_LINE_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_SSE_EVENT_BYTES: usize = 256 * 1024;
pub const MAX_SSE_EVENT_NAME_BYTES: usize = 128;

#[derive(Clone, Copy, Debug)]
pub struct SseDiscardPolicy {
    pub should_discard: fn(&str) -> bool,
    pub max_event_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SseLimits {
    pub max_line_bytes: usize,
    pub max_event_bytes: usize,
}

impl Default for SseLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: DEFAULT_MAX_SSE_LINE_BYTES,
            max_event_bytes: DEFAULT_MAX_SSE_EVENT_BYTES,
        }
    }
}

impl SseLimits {
    pub fn validate(self) -> Result<Self, SseError> {
        if self.max_line_bytes == 0
            || self.max_event_bytes == 0
            || self.max_line_bytes > self.max_event_bytes
        {
            return Err(SseError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
}

#[derive(Debug)]
pub struct SseParser {
    limits: SseLimits,
    discard_policy: Option<SseDiscardPolicy>,
    pending_line: Vec<u8>,
    current_line_bytes: usize,
    discarding_data_line: bool,
    event_name: Option<String>,
    data: Vec<u8>,
    event_bytes: usize,
    skip_lf_after_cr: bool,
    finished: bool,
}

impl SseParser {
    #[allow(dead_code)]
    pub fn new(limits: SseLimits) -> Result<Self, SseError> {
        Self::new_inner(limits, None)
    }

    pub fn new_with_discard(
        limits: SseLimits,
        discard_policy: SseDiscardPolicy,
    ) -> Result<Self, SseError> {
        if discard_policy.max_event_bytes == 0 {
            return Err(SseError::InvalidLimits);
        }
        Self::new_inner(limits, Some(discard_policy))
    }

    fn new_inner(
        limits: SseLimits,
        discard_policy: Option<SseDiscardPolicy>,
    ) -> Result<Self, SseError> {
        Ok(Self {
            limits: limits.validate()?,
            discard_policy,
            pending_line: Vec::new(),
            current_line_bytes: 0,
            discarding_data_line: false,
            event_name: None,
            data: Vec::new(),
            event_bytes: 0,
            skip_lf_after_cr: false,
            finished: false,
        })
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        if self.finished {
            return Err(SseError::AlreadyFinished);
        }

        let mut events = Vec::new();
        for &byte in chunk {
            if self.skip_lf_after_cr {
                self.skip_lf_after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if matches!(byte, b'\r' | b'\n') {
                self.finish_line(&mut events)?;
                self.skip_lf_after_cr = byte == b'\r';
            } else {
                self.current_line_bytes = self
                    .current_line_bytes
                    .checked_add(1)
                    .ok_or(SseError::EventTooLarge)?;
                if self.discarding_data_line {
                    self.ensure_current_event_bound()?;
                    continue;
                }
                self.pending_line.push(byte);
                if self.should_discard_current_event() && self.pending_line == b"data:" {
                    // Hermes writes the event name before a single-line JSON
                    // payload. Once a configured sensitive event is known, do
                    // not retain or parse the raw tool arguments/results.
                    self.pending_line.clear();
                    self.discarding_data_line = true;
                    self.ensure_current_event_bound()?;
                } else if self.pending_line.len() > self.limits.max_line_bytes {
                    return Err(SseError::LineTooLarge);
                }
            }
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<SseEvent>, SseError> {
        if self.finished {
            return Err(SseError::AlreadyFinished);
        }

        let mut events = Vec::new();
        if self.current_line_bytes != 0 || self.discarding_data_line {
            self.finish_line(&mut events)?;
        }
        self.dispatch(&mut events)?;
        self.finished = true;
        Ok(events)
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.pending_line.clear();
        self.current_line_bytes = 0;
        self.discarding_data_line = false;
        self.event_name = None;
        self.data.clear();
        self.event_bytes = 0;
        self.skip_lf_after_cr = false;
        self.finished = false;
    }

    fn finish_line(&mut self, events: &mut Vec<SseEvent>) -> Result<(), SseError> {
        let line_bytes = std::mem::take(&mut self.current_line_bytes);
        if self.discarding_data_line {
            self.discarding_data_line = false;
            self.event_bytes = self
                .event_bytes
                .checked_add(line_bytes.saturating_add(1))
                .ok_or(SseError::EventTooLarge)?;
            if self.event_bytes > self.current_event_limit() {
                return Err(SseError::EventTooLarge);
            }
            return Ok(());
        }

        let line = std::mem::take(&mut self.pending_line);
        debug_assert_eq!(line.len(), line_bytes);
        self.process_line(&line, events)
    }

    fn should_discard_current_event(&self) -> bool {
        self.discard_policy
            .zip(self.event_name.as_deref())
            .is_some_and(|(policy, event)| (policy.should_discard)(event))
    }

    fn current_event_limit(&self) -> usize {
        if self.should_discard_current_event() {
            self.discard_policy
                .map(|policy| policy.max_event_bytes)
                .unwrap_or(self.limits.max_event_bytes)
        } else {
            self.limits.max_event_bytes
        }
    }

    fn ensure_current_event_bound(&self) -> Result<(), SseError> {
        let projected = self
            .event_bytes
            .checked_add(self.current_line_bytes.saturating_add(1))
            .ok_or(SseError::EventTooLarge)?;
        if projected > self.current_event_limit() {
            Err(SseError::EventTooLarge)
        } else {
            Ok(())
        }
    }

    fn process_line(&mut self, line: &[u8], events: &mut Vec<SseEvent>) -> Result<(), SseError> {
        if line.is_empty() {
            self.dispatch(events)?;
            return Ok(());
        }

        self.event_bytes = self
            .event_bytes
            .checked_add(line.len().saturating_add(1))
            .ok_or(SseError::EventTooLarge)?;
        if self.event_bytes > self.current_event_limit() {
            return Err(SseError::EventTooLarge);
        }

        if line.starts_with(b":") {
            return Ok(());
        }

        let colon = line.iter().position(|byte| *byte == b':');
        let (field, mut value) = match colon {
            Some(index) => (&line[..index], &line[index + 1..]),
            None => (line, &[][..]),
        };
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }

        match field {
            b"event" => {
                if value.is_empty() || value.len() > MAX_SSE_EVENT_NAME_BYTES {
                    return Err(SseError::InvalidEventName);
                }
                let event = std::str::from_utf8(value).map_err(|_| SseError::InvalidUtf8)?;
                if !event
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                {
                    return Err(SseError::InvalidEventName);
                }
                self.event_name = Some(event.to_string());
            }
            b"data" => {
                let separator = usize::from(!self.data.is_empty());
                let next_size = self
                    .data
                    .len()
                    .checked_add(separator)
                    .and_then(|size| size.checked_add(value.len()))
                    .ok_or(SseError::EventTooLarge)?;
                if next_size > self.limits.max_event_bytes {
                    return Err(SseError::EventTooLarge);
                }
                if separator != 0 {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
            }
            // `id`, `retry`, and extension fields do not affect the voice
            // adapter. Ignoring them avoids retaining attacker-controlled
            // metadata while event_bytes still bounds the complete event.
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) -> Result<(), SseError> {
        if !self.data.is_empty() {
            std::str::from_utf8(&self.data).map_err(|_| SseError::InvalidUtf8)?;
            let data = serde_json::from_slice(&self.data).map_err(|_| SseError::InvalidJson)?;
            events.push(SseEvent {
                event: self
                    .event_name
                    .take()
                    .unwrap_or_else(|| "message".to_string()),
                data,
            });
        }
        self.event_name = None;
        self.data.clear();
        self.event_bytes = 0;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SseError {
    InvalidLimits,
    AlreadyFinished,
    LineTooLarge,
    EventTooLarge,
    InvalidUtf8,
    InvalidEventName,
    InvalidJson,
}

impl fmt::Display for SseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLimits => "SSE parser limits are invalid",
            Self::AlreadyFinished => "SSE parser is already finished",
            Self::LineTooLarge => "SSE line exceeds its bound",
            Self::EventTooLarge => "SSE event exceeds its bound",
            Self::InvalidUtf8 => "SSE event is not valid UTF-8",
            Self::InvalidEventName => "SSE event name is invalid",
            Self::InvalidJson => "SSE data is not valid JSON",
        })
    }
}

impl std::error::Error for SseError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parser() -> SseParser {
        SseParser::new(SseLimits {
            max_line_bytes: 1_024,
            max_event_bytes: 4_096,
        })
        .unwrap()
    }

    #[test]
    fn parses_a_hermes_response_delta_split_at_every_byte_boundary() {
        let wire = concat!(
            "event: response.output_text.delta\r\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\r\n",
            "\r\n"
        );
        for split in 0..=wire.len() {
            let mut parser = parser();
            let mut events = parser.push(&wire.as_bytes()[..split]).unwrap();
            events.extend(parser.push(&wire.as_bytes()[split..]).unwrap());
            assert_eq!(
                events,
                vec![SseEvent {
                    event: "response.output_text.delta".into(),
                    data: json!({
                        "type": "response.output_text.delta",
                        "delta": "Hello"
                    }),
                }],
                "split at byte {split}"
            );
        }
    }

    #[test]
    fn ignores_comments_and_metadata_and_uses_default_event_name() {
        let mut parser = parser();
        let events = parser
            .push(b": keepalive\nid: secret-not-retained\nretry: 1000\ndata: {\"ok\":true}\n\n")
            .unwrap();
        assert_eq!(
            events,
            vec![SseEvent {
                event: "message".into(),
                data: json!({"ok": true}),
            }]
        );
    }

    #[test]
    fn joins_multiple_data_lines_before_parsing_json() {
        let mut parser = parser();
        let events = parser
            .push(b"event: custom\ndata: {\"text\":\ndata: \"hello\"}\n\n")
            .unwrap();
        assert_eq!(events[0].event, "custom");
        assert_eq!(events[0].data, json!({"text": "hello"}));
    }

    #[test]
    fn accepts_lf_crlf_and_cr_line_endings_even_across_chunks() {
        for wire in [
            "event: one\ndata: {\"ok\":true}\n\n",
            "event: one\r\ndata: {\"ok\":true}\r\n\r\n",
            "event: one\rdata: {\"ok\":true}\r\r",
        ] {
            for split in 0..=wire.len() {
                let mut parser = parser();
                let mut events = parser.push(&wire.as_bytes()[..split]).unwrap();
                events.extend(parser.push(&wire.as_bytes()[split..]).unwrap());
                assert_eq!(
                    events,
                    vec![SseEvent {
                        event: "one".into(),
                        data: json!({"ok": true}),
                    }],
                    "line ending input split at {split}"
                );
            }
        }
    }

    #[test]
    fn exposes_tool_payload_only_as_bounded_json_without_interpreting_it() {
        let mut parser = parser();
        let events = parser
            .push(
                b"event: response.output_item.added\ndata: {\"item\":{\"type\":\"function_call_output\",\"output\":\"private\"}}\n\n",
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "response.output_item.added");
        assert_eq!(events[0].data["item"]["output"], "private");
    }

    #[test]
    fn selectively_discards_large_sensitive_events_without_retaining_json() {
        fn discard_tool_event(event: &str) -> bool {
            event == "response.output_item.added"
        }

        let mut parser = SseParser::new_with_discard(
            SseLimits {
                max_line_bytes: 1_024,
                max_event_bytes: 1_024,
            },
            SseDiscardPolicy {
                should_discard: discard_tool_event,
                max_event_bytes: 256 * 1_024,
            },
        )
        .unwrap();
        let private = "x".repeat(128 * 1_024);
        let wire = format!(
            "event: response.output_item.added\ndata: {{\"private\":\"{private}\"}}\n\nevent: response.completed\ndata: {{\"status\":\"completed\"}}\n\n"
        );

        let mut events = Vec::new();
        for chunk in wire.as_bytes().chunks(137) {
            events.extend(parser.push(chunk).unwrap());
            assert!(parser.data.is_empty());
            assert!(parser.pending_line.len() <= "event: response.output_item.added".len());
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "response.completed");
        assert_eq!(events[0].data, json!({"status": "completed"}));
    }

    #[test]
    fn still_bounds_discarded_sensitive_events() {
        fn discard_all(_event: &str) -> bool {
            true
        }

        let mut parser = SseParser::new_with_discard(
            SseLimits {
                max_line_bytes: 32,
                max_event_bytes: 64,
            },
            SseDiscardPolicy {
                should_discard: discard_all,
                max_event_bytes: 128,
            },
        )
        .unwrap();
        let wire = format!("event: tool\ndata: {}", "x".repeat(128));
        assert_eq!(parser.push(wire.as_bytes()), Err(SseError::EventTooLarge));
    }

    #[test]
    fn finish_dispatches_an_event_without_a_trailing_blank_line() {
        let mut parser = parser();
        assert!(parser
            .push(b"event: response.completed\ndata: {\"status\":\"completed\"}")
            .unwrap()
            .is_empty());
        let events = parser.finish().unwrap();
        assert_eq!(events[0].event, "response.completed");
        assert_eq!(events[0].data["status"], "completed");
        assert_eq!(parser.finish(), Err(SseError::AlreadyFinished));
    }

    #[test]
    fn reset_discards_partial_sensitive_data_and_reuses_parser() {
        let mut parser = parser();
        parser
            .push(b"event: tool\ndata: {\"secret\":\"partial")
            .unwrap();
        parser.reset();
        let events = parser
            .push(b"event: safe\ndata: {\"ok\":true}\n\n")
            .unwrap();
        assert_eq!(events[0].event, "safe");
        assert_eq!(events[0].data, json!({"ok": true}));
    }

    #[test]
    fn rejects_invalid_json_utf8_and_event_names() {
        let mut invalid_json = parser();
        assert_eq!(
            invalid_json.push(b"data: not-json\n\n"),
            Err(SseError::InvalidJson)
        );

        let mut invalid_utf8 = parser();
        assert_eq!(
            invalid_utf8.push(b"data: \"\xff\"\n\n"),
            Err(SseError::InvalidUtf8)
        );

        let mut invalid_name = parser();
        assert_eq!(
            invalid_name.push(b"event: bad name\ndata: {}\n\n"),
            Err(SseError::InvalidEventName)
        );
    }

    #[test]
    fn enforces_line_and_whole_event_bounds_including_ignored_fields() {
        let mut line_bounded = SseParser::new(SseLimits {
            max_line_bytes: 8,
            max_event_bytes: 32,
        })
        .unwrap();
        assert_eq!(line_bounded.push(b"data: 123"), Err(SseError::LineTooLarge));

        let mut event_bounded = SseParser::new(SseLimits {
            max_line_bytes: 16,
            max_event_bytes: 20,
        })
        .unwrap();
        assert_eq!(
            event_bounded.push(b": ignored-1\n: ignored-2\n"),
            Err(SseError::EventTooLarge)
        );
    }

    #[test]
    fn rejects_incoherent_limits() {
        assert_eq!(
            SseParser::new(SseLimits {
                max_line_bytes: 100,
                max_event_bytes: 10,
            })
            .unwrap_err(),
            SseError::InvalidLimits
        );
    }
}
