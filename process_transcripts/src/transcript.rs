//! Reading a Claude Code session transcript (.jsonl) into prompts and answers.
//!
//! Each line of the file is one event. Only the conversation itself is kept:
//! the prompts typed by the user and the text blocks written by Claude. Tool
//! calls, tool results, thinking, hook output and bookkeeping events are dropped.
//! The conversation is read by following the parentUuid chain back from the
//! last message, so branches abandoned by a rewind do not appear.

use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

pub struct Turn {
    pub prompt: String,
    pub timestamp: String,
    /// The text blocks of the answer, in order. Usually the last one is the
    /// conclusion and the earlier ones are narration between tool calls.
    pub answers: Vec<String>,
}

pub struct Transcript {
    pub title: Option<String>,
    pub session_ids: Vec<String>,
    pub cwd: Option<String>,
    pub turns: Vec<Turn>,
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn is_message(v: &Value) -> bool {
    matches!(str_field(v, "type"), Some("user") | Some("assistant"))
        && !v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false)
}

struct Cleaner {
    hidden_blocks: Regex,
    pasted_tags: Regex,
}

impl Cleaner {
    fn new() -> Cleaner {
        Cleaner {
            hidden_blocks: Regex::new(
                r"(?s)<(system-reminder|local-command-caveat)>.*?</(system-reminder|local-command-caveat)>",
            )
            .unwrap(),
            pasted_tags: Regex::new(r#"</?pasted_content(\s+id="[^"]*")?>"#).unwrap(),
        }
    }

    /// The text of a user prompt, or None when the event is not a typed prompt.
    fn prompt_text(&self, v: &Value) -> Option<String> {
        if v.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
            || v.get("isCompactSummary").and_then(Value::as_bool).unwrap_or(false)
        {
            return None;
        }
        let content = v.get("message")?.get("content")?;
        let mut text = String::new();
        match content {
            Value::String(s) => text.push_str(s),
            Value::Array(blocks) => {
                for b in blocks {
                    match str_field(b, "type") {
                        Some("tool_result") => return None,
                        Some("text") => {
                            if !text.is_empty() {
                                text.push_str("\n\n");
                            }
                            text.push_str(str_field(b, "text").unwrap_or(""));
                        }
                        Some("image") => text.push_str("\n[image]\n"),
                        _ => {}
                    }
                }
            }
            _ => return None,
        }
        let text = self.hidden_blocks.replace_all(&text, "");
        let text = self.pasted_tags.replace_all(&text, "");
        let text = text.trim();
        if text.is_empty()
            || text.starts_with("<command-")
            || text.starts_with("<local-command-")
            || text.starts_with("[Request interrupted by user")
        {
            return None;
        }
        Some(text.to_string())
    }
}

/// The events of the live conversation, oldest first.
fn main_chain(events: &[Value]) -> Vec<&Value> {
    let mut by_uuid: HashMap<&str, &Value> = HashMap::new();
    for e in events {
        if let Some(u) = str_field(e, "uuid") {
            by_uuid.insert(u, e);
        }
    }
    let Some(leaf) = events.iter().rev().find(|e| is_message(e)) else {
        return Vec::new();
    };
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut current = Some(leaf);
    while let Some(e) = current {
        let uuid = str_field(e, "uuid").unwrap_or("");
        if !seen.insert(uuid) {
            break;
        }
        chain.push(e);
        // A compaction restarts the chain; logicalParentUuid links it to what came before.
        let parent = str_field(e, "parentUuid").or_else(|| str_field(e, "logicalParentUuid"));
        current = parent.and_then(|p| by_uuid.get(p).copied());
    }
    chain.reverse();
    chain
}

pub fn read(path: &Path) -> Result<Transcript, String> {
    let data = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let events: Vec<Value> = data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let mut ai_title = None;
    let mut custom_title = None;
    let mut session_ids: Vec<String> = Vec::new();
    for e in &events {
        match str_field(e, "type") {
            Some("ai-title") => ai_title = str_field(e, "aiTitle").map(String::from),
            Some("custom-title") => custom_title = str_field(e, "customTitle").map(String::from),
            _ => {}
        }
    }

    let cleaner = Cleaner::new();
    let chain = main_chain(&events);
    let mut cwd = None;
    let mut turns: Vec<Turn> = Vec::new();
    for e in chain {
        if let Some(id) = str_field(e, "sessionId")
            && !session_ids.iter().any(|s| s == id)
        {
            session_ids.push(id.to_string());
        }
        if cwd.is_none() {
            cwd = str_field(e, "cwd").map(String::from);
        }
        match str_field(e, "type") {
            Some("user") => {
                if let Some(prompt) = cleaner.prompt_text(e) {
                    turns.push(Turn {
                        prompt,
                        timestamp: str_field(e, "timestamp").unwrap_or("").to_string(),
                        answers: Vec::new(),
                    });
                }
            }
            Some("assistant") => {
                let Some(turn) = turns.last_mut() else { continue };
                let Some(Value::Array(blocks)) = e.get("message").and_then(|m| m.get("content"))
                else {
                    continue;
                };
                for b in blocks {
                    if str_field(b, "type") == Some("text") {
                        let text = str_field(b, "text").unwrap_or("").trim();
                        if !text.is_empty() && text != "No response requested." {
                            turn.answers.push(text.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if turns.is_empty() {
        return Err("no conversation found (is this a Claude Code transcript?)".to_string());
    }
    Ok(Transcript { title: custom_title.or(ai_title), session_ids, cwd, turns })
}

/// "2026-10-15T06:24:55.123Z" -> "2026-10-15 08:24" in local time.
pub fn local_time(timestamp: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(timestamp) {
        Ok(t) => t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string(),
        Err(_) => timestamp.to_string(),
    }
}
