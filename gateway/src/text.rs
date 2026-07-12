use serde_json::Value;

pub const MAX_TTS_CHARS: usize = 4_000;

/// Stateful cleanup for incrementally emitted Hermes phrases.
///
/// `prepare_for_tts` handles ordinary Markdown inside one phrase. This layer
/// additionally remembers fenced-code state and partial backtick delimiters
/// across phrase boundaries, so a long code block cannot become speakable
/// merely because the phrase segmenter split it.
#[derive(Debug, Default)]
pub struct StreamingTtsSanitizer {
    in_code_fence: bool,
    pending_backticks: u8,
}

impl StreamingTtsSanitizer {
    pub fn push_phrase(&mut self, phrase: &str) -> String {
        let mut visible = String::with_capacity(phrase.len());
        for character in phrase.chars() {
            if character == '`' {
                self.pending_backticks = self.pending_backticks.saturating_add(1);
                if self.pending_backticks == 3 {
                    self.in_code_fence = !self.in_code_fence;
                    self.pending_backticks = 0;
                }
                continue;
            }

            // One or two backticks are inline-code markup. Strip the markers
            // but retain their content when not inside a fenced block.
            self.pending_backticks = 0;
            if !self.in_code_fence {
                visible.push(character);
            }
        }
        prepare_for_tts(&visible)
    }

    pub fn reset(&mut self) {
        self.in_code_fence = false;
        self.pending_backticks = 0;
    }
}

pub fn extract_hermes_response(value: &Value) -> Option<String> {
    if value.get("object").and_then(Value::as_str) != Some("response")
        || value.get("status").and_then(Value::as_str) != Some("completed")
    {
        return None;
    }

    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }

    let mut parts = Vec::new();
    for item in value.get("output")?.as_array()? {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            let kind = part.get("type").and_then(Value::as_str);
            if !matches!(kind, Some("output_text" | "text")) {
                continue;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                let text = text.trim();
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
    }

    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub fn clean_for_tts(markdown: &str) -> String {
    let mut without_code_blocks = String::new();
    let mut in_fence = false;

    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        let line = strip_line_prefix(trimmed);
        if !line.is_empty() {
            without_code_blocks.push_str(line);
            without_code_blocks.push(' ');
        }
    }

    let without_links = replace_markdown_links(&without_code_blocks);
    let without_markup: String = without_links
        .chars()
        .filter(|character| !matches!(character, '`' | '*' | '_' | '~'))
        .collect();

    without_markup
        .split_whitespace()
        .filter(|token| !is_bare_http_url(token))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn prepare_for_tts(markdown: &str) -> String {
    cap_tts_chars(&clean_for_tts(markdown), MAX_TTS_CHARS)
}

/// Prepare a response without changing its meaning through truncation.
/// Production call sites use this strict variant so an over-limit response is
/// reported as an error instead of being presented as a successful partial
/// answer.
pub fn prepare_for_tts_strict(markdown: &str) -> Option<String> {
    let cleaned = clean_for_tts(markdown);
    (!cleaned.is_empty() && cleaned.chars().count() <= MAX_TTS_CHARS).then_some(cleaned)
}

fn cap_tts_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut iterator = text.chars();
    let mut characters: Vec<char> = iterator.by_ref().take(max_chars).collect();
    if iterator.next().is_none() {
        return text.to_string();
    }

    if max_chars == 1 {
        return "…".to_string();
    }

    characters.truncate(max_chars - 1);
    let minimum_word_boundary = (max_chars - 1).saturating_mul(3) / 4;
    if let Some(position) = characters
        .iter()
        .rposition(|character| character.is_whitespace())
        .filter(|position| *position >= minimum_word_boundary)
    {
        characters.truncate(position);
    }
    while characters
        .last()
        .is_some_and(|character| character.is_whitespace())
    {
        characters.pop();
    }
    characters.push('…');
    characters.into_iter().collect()
}

fn is_bare_http_url(token: &str) -> bool {
    let token = token.trim_start_matches(|character: char| {
        matches!(character, '(' | '[' | '{' | '<' | '"' | '\'')
    });
    let lower = token.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn strip_line_prefix(mut line: &str) -> &str {
    while let Some(rest) = line.strip_prefix('#') {
        line = rest;
    }
    line = line.trim_start();

    for prefix in ["> ", "- ", "+ ", "* "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest.trim_start();
        }
    }

    let digit_count = line.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count > 0 && line.get(digit_count..digit_count + 2) == Some(". ") {
        return line[digit_count + 2..].trim_start();
    }

    line
}

fn replace_markdown_links(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut cursor = 0_usize;

    while cursor < input.len() {
        let remaining = &input[cursor..];
        let (prefix_len, label_start) = if remaining.starts_with("![") {
            (2, cursor + 2)
        } else if remaining.starts_with('[') {
            (1, cursor + 1)
        } else {
            let character = remaining.chars().next().expect("cursor on char boundary");
            result.push(character);
            cursor += character.len_utf8();
            continue;
        };

        let Some(label_close_relative) = input[label_start..].find("](") else {
            result.push_str(&input[cursor..cursor + prefix_len]);
            cursor += prefix_len;
            continue;
        };
        let label_close = label_start + label_close_relative;
        let url_start = label_close + 2;
        let Some(url_close_relative) = input[url_start..].find(')') else {
            result.push_str(&input[cursor..cursor + prefix_len]);
            cursor += prefix_len;
            continue;
        };
        let url_close = url_start + url_close_relative;
        result.push_str(&input[label_start..label_close]);
        cursor = url_close + 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_output_text_from_hermes_response() {
        let response = json!({
            "object": "response",
            "status": "completed",
            "output": [
                {"type": "function_call", "name": "terminal"},
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": " First sentence. "},
                        {"type": "output_text", "text": "Second sentence."}
                    ]
                }
            ]
        });
        assert_eq!(
            extract_hermes_response(&response).as_deref(),
            Some("First sentence.\nSecond sentence.")
        );
    }

    #[test]
    fn accepts_top_level_output_text_fallback() {
        let response = json!({
            "object": "response",
            "status": "completed",
            "output_text": "Hello from Hermes."
        });
        assert_eq!(
            extract_hermes_response(&response).as_deref(),
            Some("Hello from Hermes.")
        );
    }

    #[test]
    fn ignores_non_message_output() {
        let response = json!({
            "object": "response",
            "status": "completed",
            "output": [{"type": "function_call_output", "output": "secret"}]
        });
        assert_eq!(extract_hermes_response(&response), None);
    }

    #[test]
    fn rejects_incomplete_or_non_response_envelopes() {
        assert_eq!(
            extract_hermes_response(&json!({
                "object": "response",
                "status": "in_progress",
                "output_text": "Do not speak yet"
            })),
            None
        );
        assert_eq!(
            extract_hermes_response(&json!({
                "object": "chat.completion",
                "status": "completed",
                "output_text": "Wrong API shape"
            })),
            None
        );
    }

    #[test]
    fn cleans_markdown_for_speech() {
        let markdown = r#"
# Result

- Read the **[deployment guide](https://example.com/guide)**.
- Run `cargo test` locally.

```rust
println!("do not speak this block");
```

> Ready when you are.
"#;
        assert_eq!(
            clean_for_tts(markdown),
            "Result Read the deployment guide. Run cargo test locally. Ready when you are."
        );
    }

    #[test]
    fn preserves_unicode_during_cleanup() {
        assert_eq!(
            clean_for_tts("**Café** — [你好](https://example.com)"),
            "Café — 你好"
        );
    }

    #[test]
    fn strips_bare_http_urls_but_keeps_markdown_link_labels() {
        assert_eq!(
            clean_for_tts(
                "Read [the guide](https://example.com/guide), then visit https://example.com/raw or (HTTP://example.org)."
            ),
            "Read the guide, then visit or"
        );
    }

    #[test]
    fn caps_tts_text_on_unicode_boundaries() {
        let capped = cap_tts_chars("你好世界 alpha beta gamma", 10);
        assert_eq!(capped.chars().count(), 10);
        assert_eq!(capped, "你好世界 alph…");
        assert_eq!(cap_tts_chars("你好", 4), "你好");
        assert_eq!(cap_tts_chars("anything", 1), "…");
    }

    #[test]
    fn prepared_tts_never_exceeds_the_gateway_limit() {
        let oversized = "word ".repeat(MAX_TTS_CHARS);
        let prepared = prepare_for_tts(&oversized);
        assert!(prepared.chars().count() <= MAX_TTS_CHARS);
        assert!(prepared.ends_with('…'));
    }

    #[test]
    fn strict_tts_preparation_rejects_instead_of_truncating() {
        assert_eq!(
            prepare_for_tts_strict("**Short answer.**"),
            Some("Short answer.".into())
        );
        assert_eq!(prepare_for_tts_strict("```rust\nhidden\n```"), None);
        assert_eq!(prepare_for_tts_strict(&"x".repeat(MAX_TTS_CHARS + 1)), None);
    }

    #[test]
    fn streaming_sanitizer_keeps_code_fences_closed_across_phrases() {
        let mut sanitizer = StreamingTtsSanitizer::default();
        assert_eq!(
            sanitizer.push_phrase("The result is ready. ```rust\nlet secret = 42;"),
            "The result is ready."
        );
        assert_eq!(
            sanitizer.push_phrase("println!(\"still hidden\");\n``` Continue safely."),
            "Continue safely."
        );
    }

    #[test]
    fn streaming_sanitizer_handles_a_split_fence_delimiter() {
        let mut sanitizer = StreamingTtsSanitizer::default();
        assert_eq!(sanitizer.push_phrase("Speak this. ``"), "Speak this.");
        assert_eq!(sanitizer.push_phrase("`hidden code"), "");
        assert_eq!(sanitizer.push_phrase("``` Back again."), "Back again.");
        sanitizer.reset();
        assert_eq!(
            sanitizer.push_phrase("Visible after reset."),
            "Visible after reset."
        );
    }
}
