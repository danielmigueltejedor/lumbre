//! The streaming format of an OpenAI-compatible `/v1/chat/completions` endpoint, which is what
//! llama.cpp's server speaks: server-sent events, one JSON chunk per `data:` line.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Chunk {
    /// Visible answer text.
    Text(String),
    /// Token accounting, sent in the last chunk when the server supports it.
    Usage { prompt_tokens: Option<u32>, completion_tokens: Option<u32> },
    /// `data: [DONE]`.
    Done,
}

/// Parses one line of the stream. Comments, blank lines, and chunks without visible text (role
/// announcements, reasoning deltas) yield nothing.
pub(crate) fn parse_line(line: &str) -> Result<Vec<Chunk>, String> {
    let Some(data) = line.strip_prefix("data:") else { return Ok(Vec::new()) };
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(vec![Chunk::Done]);
    }
    let json: Value = serde_json::from_str(data).map_err(|error| format!("{error} in `{data}`"))?;
    if let Some(message) = json.pointer("/error/message").and_then(Value::as_str) {
        return Err(message.to_owned());
    }
    let mut chunks = Vec::new();
    if let Some(text) = json.pointer("/choices/0/delta/content").and_then(Value::as_str) {
        if !text.is_empty() {
            chunks.push(Chunk::Text(text.to_owned()));
        }
    }
    if let Some(usage) = json.get("usage").filter(|usage| usage.is_object()) {
        let count = |key: &str| usage.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
        chunks.push(Chunk::Usage {
            prompt_tokens: count("prompt_tokens"),
            completion_tokens: count("completion_tokens"),
        });
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_usage_and_done() {
        assert_eq!(
            parse_line(r#"data: {"choices":[{"delta":{"content":"Hola"}}]}"#),
            Ok(vec![Chunk::Text("Hola".into())])
        );
        assert_eq!(
            parse_line(r#"data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":34}}"#),
            Ok(vec![Chunk::Usage { prompt_tokens: Some(12), completion_tokens: Some(34) }])
        );
        assert_eq!(parse_line("data: [DONE]"), Ok(vec![Chunk::Done]));
    }

    #[test]
    fn noise_is_ignored_and_errors_surface() {
        assert_eq!(parse_line(""), Ok(vec![]));
        assert_eq!(parse_line(": keep-alive"), Ok(vec![]));
        assert_eq!(
            parse_line(r#"data: {"choices":[{"delta":{"role":"assistant","content":null}}]}"#),
            Ok(vec![])
        );
        // Reasoning ("thinking") deltas are not part of the visible answer.
        assert_eq!(parse_line(r#"data: {"choices":[{"delta":{"reasoning_content":"hmm"}}]}"#), Ok(vec![]));
        assert_eq!(
            parse_line(r#"data: {"error":{"message":"context too long"}}"#),
            Err("context too long".into())
        );
        assert!(parse_line("data: {not json").is_err());
    }
}
