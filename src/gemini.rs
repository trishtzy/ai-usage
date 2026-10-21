//! Reads token usage from Gemini CLI's chat recordings.
//!
//! Gemini CLI records each session under `tmp/<project>/chats/`, as JSONL
//! appended to as the session goes: a header, then message lines, with
//! subagent sessions in a folder named after their parent. Sessions from
//! before April 2026 are one JSON object holding every message, rewritten
//! on each save. A response is a message of type `gemini` whose `tokens`
//! hold the prompt (cached tokens included), output, thoughts, and tool-use
//! prompt.
//!
//! A message is written again each time it changes, and for a while every
//! history sync wrote a `$set` line with the whole history, so responses
//! are counted once by `id`, from the first copy that carries tokens.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::usage::{Card, Meter, Record, Tokens};

pub const PROVIDER: &str = "gemini";

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "$set")]
    set: Option<Session>,
    #[serde(flatten)]
    message: Message,
}

/// A whole session from before April 2026, or a `$set` line's history.
#[derive(Deserialize)]
struct Session {
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    tokens: Option<Usage>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default)]
struct Usage {
    input: u64,
    output: u64,
    cached: u64,
    thoughts: u64,
    tool: u64,
}

/// `$GEMINI_CLI_HOME/.gemini`, with `GEMINI_CLI_HOME` defaulting to `~`.
pub fn default_dir() -> Option<PathBuf> {
    let home = std::env::var_os("GEMINI_CLI_HOME").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".gemini"))
}

/// Returns every response recorded under `dir` at or after `since`. A
/// missing directory means Gemini CLI isn't installed and yields no records.
pub fn records(dir: &Path, since: Option<DateTime<Utc>>) -> Vec<Record> {
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    let mut add = |message: Message| {
        let (Some(id), Some(timestamp), Some(usage)) =
            (message.id, message.timestamp, message.tokens)
        else {
            return;
        };
        if message.kind.as_deref() != Some("gemini")
            || since.is_some_and(|since| timestamp < since)
            || !seen.insert(id)
        {
            return;
        }
        records.push(Record {
            provider: PROVIDER,
            model: message.model.unwrap_or_else(|| "unknown".to_string()),
            timestamp,
            tokens: Tokens {
                input: usage.input.saturating_sub(usage.cached) + usage.tool,
                // Codex counts reasoning as output, so this does too.
                output: usage.output + usage.thoughts,
                cache_write: 0,
                cache_read: usage.cached,
            },
        });
    };
    for entry in WalkDir::new(dir.join("tmp"))
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        let in_chats = path
            .ancestors()
            .any(|a| a.file_name().is_some_and(|n| n == "chats"));
        let extension = path.extension().and_then(|e| e.to_str());
        if !entry.file_type().is_file() || !in_chats {
            continue;
        }
        // A file can't hold a message newer than its last write.
        if let Some(since) = since {
            let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
            if modified.is_some_and(|m| m < SystemTime::from(since)) {
                continue;
            }
        }
        match extension {
            Some("jsonl") => {
                let Ok(file) = File::open(path) else {
                    continue;
                };
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    if !line.contains("\"tokens\"") {
                        continue;
                    }
                    // The file of a running session can end in a half-written line.
                    let Ok(line) = serde_json::from_str::<Line>(&line) else {
                        continue;
                    };
                    for message in line.set.map(|s| s.messages).unwrap_or_default() {
                        add(message);
                    }
                    add(line.message);
                }
            }
            Some("json") => {
                let session = std::fs::read(path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Session>(&bytes).ok());
                for message in session.map(|s| s.messages).unwrap_or_default() {
                    add(message);
                }
            }
            _ => {}
        }
    }
    records
}

/// The Gemini card for `ai-usage status`: tokens used since `today`. Gemini
/// CLI keeps no plan limits on disk. `None` when it hasn't been used today.
pub fn card(dir: &Path, today: DateTime<Utc>) -> Option<Card> {
    let used: u64 = records(dir, Some(today))
        .iter()
        .map(|r| r.tokens.input + r.tokens.output)
        .sum();
    if used == 0 {
        return None;
    }
    Some(Card {
        provider: PROVIDER,
        title: "Gemini CLI",
        as_of: None,
        meters: vec![Meter::today(used)],
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn response(id: &str, ts: &str, tokens: &str) -> String {
        format!(
            r#"{{"id":"{id}","timestamp":"{ts}","type":"gemini","content":"","model":"gemini-3-pro","tokens":{tokens}}}"#
        )
    }

    const TOKENS: &str =
        r#"{"input":1000,"output":20,"cached":600,"thoughts":5,"tool":3,"total":1028}"#;

    #[test]
    fn counts_each_response_once() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("tmp/my-app/chats");
        fs::create_dir_all(chats.join("parent-id")).unwrap();
        let a = response("a", "2026-10-21T09:00:00.000Z", TOKENS);
        let lines = [
            r#"{"sessionId":"s1","projectHash":"h","startTime":"2026-10-21T08:59:00.000Z","lastUpdated":"2026-10-21T08:59:00.000Z"}"#.to_string(),
            r#"{"id":"u","timestamp":"2026-10-21T09:00:00.000Z","type":"user","content":"hi"}"#.to_string(),
            // Written before its tokens arrived, then again with them.
            response("a", "2026-10-21T09:00:00.000Z", "null"),
            a.clone(),
            format!(r#"{{"$set":{{"lastUpdated":"2026-10-21T09:01:00.000Z","messages":[{a}]}}}}"#),
            "{\"id\":\"b\",\"timestamp\":\"2026-10-21T09:02:0".to_string(),
        ];
        fs::write(
            chats.join("session-2026-10-21T08-59-s1.jsonl"),
            lines.join("\n"),
        )
        .unwrap();
        fs::write(
            chats.join("parent-id/sub.jsonl"),
            response(
                "c",
                "2026-10-21T09:05:00.000Z",
                r#"{"input":10,"output":1,"cached":0}"#,
            ),
        )
        .unwrap();
        // An older session, as one JSON object.
        fs::write(
            chats.join("session-2026-03-01T10-00-old.json"),
            format!(
                r#"{{"sessionId":"old","messages":[{}]}}"#,
                response("d", "2026-03-01T10:00:00.000Z", TOKENS)
            ),
        )
        .unwrap();

        let mut got = records(dir.path(), None);
        got.sort_by_key(|r| r.timestamp);
        let ids: Vec<_> = got.iter().map(|r| r.timestamp.to_rfc3339()).collect();
        assert_eq!(ids.len(), 3, "{ids:?}");
        assert_eq!(
            got[1].tokens,
            Tokens {
                input: 403,
                output: 25,
                cache_write: 0,
                cache_read: 600
            }
        );

        let today = "2026-10-21T00:00:00Z".parse().unwrap();
        assert_eq!(
            card(dir.path(), today).unwrap().meters[0].value,
            "439 tokens"
        );
        assert_eq!(card(&dir.path().join("absent"), today), None);
    }
}
