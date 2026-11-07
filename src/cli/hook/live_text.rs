//! Fork-local: while an agent is running, its sidebar row shows a piece of
//! the newest text the agent wrote in the current turn, not the tool it
//! calls. The text comes from the Claude Code transcript (JSONL) named by
//! the hook payload's `transcript_path`, read from the tail only.

use std::io::{Read, Seek, SeekFrom};

use crate::tmux;

use super::super::sanitize_tmux_value;
use super::context::pane_writes_allowed;

/// How much of the transcript tail is read on each tool call.
const TAIL_BYTES: u64 = 256 * 1024;
/// Longest text stored in the pane option; the row shows three lines at most.
const MAX_CHARS: usize = 400;

/// Newest assistant text of the current turn in JSONL `content`, or `None`
/// when the agent has not written any text since the user's last prompt.
/// Tool results arrive as `user` lines too; only a real prompt ends the turn.
pub(super) fn latest_assistant_text(content: &str) -> Option<String> {
    for line in content.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(|s| s.as_bool()) == Some(true) {
            continue;
        }
        let message = v.get("message");
        match v.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let texts: Vec<&str> = message
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter(|i| i.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                            .filter(|t| !t.trim().is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(text) = texts.last() {
                    return Some(compact(text));
                }
            }
            Some("user") if is_real_prompt(message) => return None,
            _ => {}
        }
    }
    None
}

fn is_real_prompt(message: Option<&serde_json::Value>) -> bool {
    match message.and_then(|m| m.get("content")) {
        Some(serde_json::Value::String(s)) => !s.trim().is_empty(),
        Some(serde_json::Value::Array(items)) => !items
            .iter()
            .any(|i| i.get("type").and_then(|t| t.as_str()) == Some("tool_result")),
        _ => false,
    }
}

/// One line of plain text: whitespace collapsed, markdown emphasis and
/// backticks dropped, capped at `MAX_CHARS` characters.
fn compact(text: &str) -> String {
    let plain: String = text.chars().filter(|c| !matches!(c, '*' | '`')).collect();
    let words: Vec<&str> = plain.split_whitespace().collect();
    let joined = words.join(" ");
    joined.chars().take(MAX_CHARS).collect()
}

fn read_tail(path: &str) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    // The first line is usually cut in half by the seek; drop it.
    if start > 0 {
        text.split_once('\n').map(|(_, rest)| rest.to_string())
    } else {
        Some(text)
    }
}

/// Write the newest assistant text of the turn into `@pane_prompt` as a
/// response, so a running row shows what the agent is saying now.
pub(super) fn update_from_transcript(pane: &str, transcript_path: &str) {
    if transcript_path.is_empty() || !pane_writes_allowed(pane) {
        return;
    }
    let Some(tail) = read_tail(transcript_path) else {
        return;
    };
    let Some(text) = latest_assistant_text(&tail) else {
        return;
    };
    let text = sanitize_tmux_value(&text);
    if tmux::get_pane_option_value(pane, tmux::PANE_PROMPT) == text {
        return;
    }
    tmux::set_pane_option(pane, tmux::PANE_PROMPT, &text);
    tmux::set_pane_option(pane, tmux::PANE_PROMPT_SOURCE, "response");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_prompt(text: &str) -> String {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": text}})
            .to_string()
    }

    fn tool_result() -> String {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
        ]}})
        .to_string()
    }

    fn assistant(blocks: serde_json::Value) -> String {
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": blocks}})
            .to_string()
    }

    fn jsonl(lines: &[String]) -> String {
        lines.join("\n") + "\n"
    }

    #[test]
    fn newest_text_of_the_turn_wins_over_tool_calls_after_it() {
        let t = jsonl(&[
            user_prompt("oprav sidebar"),
            assistant(serde_json::json!([{"type": "text", "text": "Najprv pozriem kód."}])),
            assistant(serde_json::json!([{"type": "tool_use", "name": "Bash", "input": {}}])),
            tool_result(),
            assistant(
                serde_json::json!([{"type": "text", "text": "Chyba je v **row.rs**."},
                                          {"type": "tool_use", "name": "Edit", "input": {}}]),
            ),
            tool_result(),
        ]);
        assert_eq!(
            latest_assistant_text(&t).as_deref(),
            Some("Chyba je v row.rs.")
        );
    }

    #[test]
    fn no_text_since_the_last_prompt_gives_none() {
        let t = jsonl(&[
            assistant(serde_json::json!([{"type": "text", "text": "Hotovo z minulého ťahu."}])),
            user_prompt("ďalšia vec"),
            assistant(serde_json::json!([{"type": "tool_use", "name": "Read", "input": {}}])),
            tool_result(),
        ]);
        assert_eq!(latest_assistant_text(&t), None);
    }

    #[test]
    fn text_is_one_line_and_capped() {
        let long = format!("prvý riadok\n\n  druhý `kód`\n{}", "x".repeat(1000));
        let t = jsonl(&[
            user_prompt("a"),
            assistant(serde_json::json!([{"type": "text", "text": long}])),
        ]);
        let got = latest_assistant_text(&t).unwrap();
        assert!(got.starts_with("prvý riadok druhý kód x"), "got: {got}");
        assert!(!got.contains('\n'));
        assert_eq!(got.chars().count(), MAX_CHARS);
    }

    #[test]
    fn sidechain_and_broken_lines_are_skipped() {
        let side = serde_json::json!({"type": "assistant", "isSidechain": true,
            "message": {"content": [{"type": "text", "text": "subagent"}]}})
        .to_string();
        let t = jsonl(&[
            user_prompt("a"),
            assistant(serde_json::json!([{"type": "text", "text": "hlavný agent"}])),
            side,
            "{not json".to_string(),
        ]);
        assert_eq!(latest_assistant_text(&t).as_deref(), Some("hlavný agent"));
    }
}
