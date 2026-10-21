//! Reads token usage from Qwen Code's usage ledger.
//!
//! Since June 2026, Qwen Code appends one line per model response, subagents
//! included, to `usage/token-usage-YYYY-MM.jsonl` in its runtime directory,
//! as long as usage statistics are on (the default). Its fields reuse
//! Gemini's names, but with an OpenAI-compatible provider `outputTokens`
//! already includes reasoning while with Gemini it doesn't, and
//! `thoughtsTokens` may be an estimate. Output is therefore taken as the
//! total less the input, which includes reasoning either way. Input
//! includes cached tokens.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::usage::{Card, Meter, Record, Tokens};

pub const PROVIDER: &str = "qwen";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    timestamp: DateTime<Utc>,
    model: Option<String>,
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

impl Entry {
    fn tokens(&self) -> Tokens {
        let output = if self.total_tokens > self.input_tokens {
            self.total_tokens - self.input_tokens
        } else {
            self.output_tokens
        };
        Tokens {
            input: self.input_tokens.saturating_sub(self.cached_tokens),
            output,
            cache_write: 0,
            cache_read: self.cached_tokens,
        }
    }
}

/// Qwen Code's runtime directory: `$QWEN_RUNTIME_DIR`, otherwise
/// `$QWEN_HOME`, defaulting to `~/.qwen`. A runtime directory set in
/// Qwen's settings instead needs `--qwen-dir`.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QWEN_RUNTIME_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("QWEN_HOME") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".qwen"))
}

/// Returns every response in the ledger under `dir` made at or after
/// `since`. A missing ledger yields no records.
pub fn records(dir: &Path, since: Option<DateTime<Utc>>) -> Vec<Record> {
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir.join("usage")) else {
        return records;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("token-usage-") && name.ends_with(".jsonl"))
        })
        .collect();
    paths.sort();
    for path in paths {
        // A month's file can't hold a response newer than its last write.
        if let Some(since) = since {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            if modified.is_some_and(|m| m < SystemTime::from(since)) {
                continue;
            }
        }
        let Ok(file) = File::open(&path) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            // The file can end in a half-written line.
            let Ok(entry) = serde_json::from_str::<Entry>(&line) else {
                continue;
            };
            if since.is_some_and(|since| entry.timestamp < since) || !seen.insert(entry.id.clone())
            {
                continue;
            }
            records.push(Record {
                provider: PROVIDER,
                tokens: entry.tokens(),
                model: entry.model.unwrap_or_else(|| "unknown".to_string()),
                timestamp: entry.timestamp,
            });
        }
    }
    records
}

/// The Qwen card for `ai-usage status`: tokens used since `today`. Qwen
/// Code keeps no plan limits on disk. `None` when it hasn't been used today.
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
        title: "Qwen Code",
        as_of: None,
        meters: vec![Meter::today(used)],
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn entry(id: &str, ts: &str, input: u64, output: u64, cached: u64, total: u64) -> String {
        format!(
            r#"{{"schemaVersion":1,"id":"{id}","timestamp":"{ts}","localDate":"2026-10-21","localMonth":"2026-10","sessionId":"s","model":"qwen3-coder-plus","authType":"qwen-oauth","source":"main","inputTokens":{input},"outputTokens":{output},"cachedTokens":{cached},"thoughtsTokens":7,"totalTokens":{total},"apiDurationMs":900}}"#
        )
    }

    #[test]
    fn reads_the_ledger_once_per_response() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("usage")).unwrap();
        let lines = [
            // Gemini-style: thoughts outside output, but inside the total.
            entry("a", "2026-10-21T09:00:00.000Z", 1000, 40, 600, 1047),
            entry("a", "2026-10-21T09:00:00.000Z", 1000, 40, 600, 1047),
            // No total reported.
            entry("b", "2026-10-21T10:00:00.000Z", 100, 30, 0, 0),
            entry("c", "2026-10-20T10:00:00.000Z", 5, 5, 0, 10),
            "{\"id\":\"d\",\"timest".to_string(),
        ];
        fs::write(
            dir.path().join("usage/token-usage-2026-10.jsonl"),
            lines.join("\n"),
        )
        .unwrap();
        fs::write(
            dir.path().join("usage/other.jsonl"),
            entry("e", "2026-10-21T09:00:00Z", 1, 1, 0, 2),
        )
        .unwrap();

        let mut got = records(dir.path(), None);
        got.sort_by_key(|r| r.timestamp);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[1].tokens,
            Tokens {
                input: 400,
                output: 47,
                cache_write: 0,
                cache_read: 600
            }
        );
        assert_eq!(got[2].tokens.output, 30);

        let today = "2026-10-21T00:00:00Z".parse().unwrap();
        assert_eq!(
            card(dir.path(), today).unwrap().meters[0].value,
            "577 tokens"
        );
        assert_eq!(card(&dir.path().join("absent"), today), None);
    }
}
