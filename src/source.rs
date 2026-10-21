//! Reads token usage that something else has logged, for providers
//! ai-usage has no reader of its own for, such as an API used directly or
//! through a private router.
//!
//! A source is a directory of `.jsonl` files, each line an API response as
//! it came back, or a wrapper holding it under `response`. Usage is read in
//! any of three shapes:
//!
//! - OpenAI chat completions, and compatible APIs such as DeepSeek's:
//!   `prompt_tokens` includes cached tokens, given as
//!   `prompt_tokens_details.cached_tokens` or `prompt_cache_hit_tokens`.
//! - OpenAI responses: `input_tokens` includes cached tokens, given as
//!   `input_tokens_details.cached_tokens`.
//! - Anthropic messages: `input_tokens` leaves out cached tokens, given as
//!   `cache_read_input_tokens` and `cache_creation_input_tokens`.
//!
//! A line's time is its `timestamp` (RFC 3339 or Unix seconds), else the
//! response's `created` or `created_at`. Anthropic responses carry no time,
//! so they need the wrapper. Responses are counted once by `id`.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde_json::Value;
use walkdir::WalkDir;

use crate::usage::{Card, Meter, Record, Tokens};

/// A provider the user names, and the directory its responses are logged to.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    /// Leaked once when parsed, so records can name it like any provider.
    pub name: &'static str,
    pub dir: PathBuf,
}

/// Parses `--source NAME=DIR`.
pub fn parse(s: &str, builtin: &[&str]) -> Result<Source, String> {
    let Some((name, dir)) = s.split_once('=') else {
        return Err(format!("expected NAME=DIR, got {s:?}"));
    };
    let name = name.trim();
    if name.is_empty() || dir.is_empty() {
        return Err(format!("expected NAME=DIR, got {s:?}"));
    }
    if builtin.contains(&name) {
        return Err(format!("{name} is read already; pick another name"));
    }
    Ok(Source {
        name: Box::leak(name.to_string().into_boxed_str()),
        dir: PathBuf::from(dir),
    })
}

/// Returns every response logged in `source` at or after `since`.
pub fn records(source: &Source, since: Option<DateTime<Utc>>) -> Vec<Record> {
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    for entry in WalkDir::new(&source.dir).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        // A file can't hold a response newer than its last write.
        if let Some(since) = since {
            let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
            if modified.is_some_and(|m| m < SystemTime::from(since)) {
                continue;
            }
        }
        let Ok(file) = File::open(path) else {
            eprintln!("ai-usage: skipping {}: can't open it", path.display());
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            // The file can end in a half-written line.
            let Ok(line) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(record) = record(source.name, &line) else {
                continue;
            };
            if since.is_some_and(|since| record.timestamp < since) {
                continue;
            }
            let id = line["response"]["id"].as_str().or(line["id"].as_str());
            if id.is_some_and(|id| !seen.insert(id.to_string())) {
                continue;
            }
            records.push(record);
        }
    }
    records
}

/// Reads one logged line, or `None` if it has no usage or no time.
fn record(provider: &'static str, line: &Value) -> Option<Record> {
    let response = if line["response"].is_object() {
        &line["response"]
    } else {
        line
    };
    let usage = response.get("usage").or(line.get("usage"))?;
    let timestamp = time(&line["timestamp"])
        .or_else(|| time(&response["timestamp"]))
        .or_else(|| time(&response["created"]))
        .or_else(|| time(&response["created_at"]))?;
    let model = response["model"]
        .as_str()
        .or(line["model"].as_str())
        .unwrap_or("unknown");
    Some(Record {
        provider,
        model: model.to_string(),
        timestamp,
        tokens: tokens(usage)?,
    })
}

/// A time given as RFC 3339 or Unix seconds.
fn time(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => DateTime::from_timestamp(n.as_i64()?, 0),
        _ => None,
    }
}

fn tokens(usage: &Value) -> Option<Tokens> {
    let n = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    if usage.get("prompt_tokens").is_some() {
        let cached = n("/prompt_tokens_details/cached_tokens").max(n("/prompt_cache_hit_tokens"));
        return Some(Tokens {
            input: n("/prompt_tokens").saturating_sub(cached),
            output: n("/completion_tokens"),
            cache_write: 0,
            cache_read: cached,
        });
    }
    if usage.get("input_tokens").is_some() {
        if usage.get("input_tokens_details").is_some() {
            let cached = n("/input_tokens_details/cached_tokens");
            return Some(Tokens {
                input: n("/input_tokens").saturating_sub(cached),
                output: n("/output_tokens"),
                cache_write: 0,
                cache_read: cached,
            });
        }
        return Some(Tokens {
            input: n("/input_tokens"),
            output: n("/output_tokens"),
            cache_write: n("/cache_creation_input_tokens"),
            cache_read: n("/cache_read_input_tokens"),
        });
    }
    None
}

/// The card for `ai-usage status`: tokens used since `today`. `None` when
/// nothing was logged today.
pub fn card(source: &Source, today: DateTime<Utc>) -> Option<Card> {
    let used: u64 = records(source, Some(today))
        .iter()
        .map(|r| r.tokens.input + r.tokens.output)
        .sum();
    if used == 0 {
        return None;
    }
    Some(Card {
        provider: source.name,
        title: source.name,
        as_of: None,
        meters: vec![Meter::today(used)],
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parses_names_and_rejects_builtins() {
        let source = parse("longwire=/var/log/longwire", &["claude"]).unwrap();
        assert_eq!(source.name, "longwire");
        assert_eq!(source.dir, PathBuf::from("/var/log/longwire"));
        assert!(parse("claude=/x", &["claude"]).is_err());
        assert!(parse("nodir=", &[]).is_err());
        assert!(parse("/just/a/path", &[]).is_err());
    }

    #[test]
    fn reads_each_usage_shape() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            // DeepSeek, as the API returned it.
            r#"{"id":"d1","object":"chat.completion","created":1792573200,"model":"deepseek-chat","usage":{"prompt_tokens":1000,"completion_tokens":50,"total_tokens":1050,"prompt_cache_hit_tokens":800,"prompt_cache_miss_tokens":200}}"#,
            // The same response logged twice.
            r#"{"id":"d1","object":"chat.completion","created":1792573200,"model":"deepseek-chat","usage":{"prompt_tokens":1000,"completion_tokens":50,"total_tokens":1050,"prompt_cache_hit_tokens":800,"prompt_cache_miss_tokens":200}}"#,
            // OpenAI-compatible, with cached tokens in the details.
            r#"{"id":"l1","created":1792573300,"model":"longwire/fast","usage":{"prompt_tokens":300,"completion_tokens":30,"prompt_tokens_details":{"cached_tokens":100}}}"#,
            // OpenAI responses.
            r#"{"id":"r1","created_at":1792573400,"model":"gpt-5","usage":{"input_tokens":500,"output_tokens":40,"input_tokens_details":{"cached_tokens":200}}}"#,
            // Anthropic, wrapped to give it a time.
            r#"{"timestamp":"2026-10-21T08:59:00Z","response":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":900,"cache_creation_input_tokens":90}}}"#,
            // Anthropic with no time: skipped.
            r#"{"id":"msg_2","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":5}}"#,
            r#"{"id":"e","error":{"message":"rate limited"}}"#,
        ];
        fs::write(dir.path().join("2026-10.jsonl"), lines.join("\n")).unwrap();
        let source = Source {
            name: "mine",
            dir: dir.path().to_path_buf(),
        };

        let mut got = records(&source, None);
        got.sort_by_key(|r| r.timestamp);
        let summary: Vec<_> = got
            .iter()
            .map(|r| {
                (
                    r.model.as_str(),
                    r.tokens.input,
                    r.tokens.output,
                    r.tokens.cache_read,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("claude-opus-5-5", 10, 5, 900),
                ("deepseek-chat", 200, 50, 800),
                ("longwire/fast", 200, 30, 100),
                ("gpt-5", 300, 40, 200),
            ]
        );
        assert_eq!(got[0].tokens.cache_write, 90);

        let today = "2026-10-21T00:00:00Z".parse().unwrap();
        let card = card(&source, today).unwrap();
        assert_eq!(card.title, "mine");
        assert_eq!(card.meters[0].value, "835 tokens");
    }
}
