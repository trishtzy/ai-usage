//! Reads token usage from Claude Code's session logs.
//!
//! Claude Code writes one JSONL file per session under `projects/<project>/`,
//! with subagent sessions in `projects/<project>/<session>/subagents/`. Each
//! assistant response is logged once per content block, every copy carrying
//! the same `usage`, and resumed sessions copy earlier responses into a new
//! file. Responses are therefore counted once per `message.id` across all
//! files.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::usage::{Card, Meter, Record, Tokens, ago};

pub mod limits;

pub const PROVIDER: &str = "claude";

/// Model name Claude Code uses for messages it makes up itself, such as
/// errors. These carry no real usage.
const SYNTHETIC_MODEL: &str = "<synthetic>";

#[derive(Deserialize)]
struct Line {
    timestamp: Option<DateTime<Utc>>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}

/// The projects directory: `$CLAUDE_CONFIG_DIR/projects` if set, otherwise
/// `~/.claude/projects`.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("projects"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude/projects"))
}

/// Returns every response logged under `dir` at or after `since`. A missing
/// directory means Claude Code isn't installed and yields no records.
pub fn records(dir: &Path, since: Option<DateTime<Utc>>) -> Vec<Record> {
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        // A file can't hold an entry newer than its last write.
        if let Some(since) = since {
            let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
            if modified.is_some_and(|m| m < SystemTime::from(since)) {
                continue;
            }
        }
        if let Err(err) = read_file(path, since, &mut seen, &mut records) {
            eprintln!("ai-usage: skipping {}: {err}", path.display());
        }
    }
    records
}

/// The Claude card for `ai-usage status`: plan limits from the last status
/// line update, and tokens used since `today`. `None` when Claude Code
/// hasn't been used since then.
pub fn card(
    dir: &Path,
    limits_path: Option<&Path>,
    today: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<Card> {
    let used: u64 = records(dir, Some(today))
        .iter()
        .map(|r| r.tokens.input + r.tokens.output)
        .sum();
    if used == 0 {
        return None;
    }
    let snapshot = limits_path.and_then(limits::load);
    let mut card = Card {
        provider: PROVIDER,
        title: "Claude Code",
        as_of: None,
        meters: Vec::new(),
        notes: Vec::new(),
    };
    match &snapshot {
        Some(snapshot) => {
            card.meters = limits::meters(snapshot, now);
            if card.meters.is_empty() {
                card.notes.push(format!(
                    "No current plan limits. Claude Code last reported them {}.",
                    ago(snapshot.as_of, now)
                ));
            } else {
                card.as_of = Some(snapshot.as_of);
            }
        }
        None => card.notes.push(
            "Plan limits show up once Claude Code runs `ai-usage claude-statusline` as its status line."
                .to_string(),
        ),
    }
    card.meters.push(Meter::today(used));
    Some(card)
}

fn read_file(
    path: &Path,
    since: Option<DateTime<Utc>>,
    seen: &mut HashSet<String>,
    records: &mut Vec<Record>,
) -> io::Result<()> {
    let reader = BufReader::new(File::open(path)?);
    for line in reader.split(b'\n') {
        let line = line?;
        // Most lines are prompts and tool output; skip them before parsing.
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        if !text.contains("\"usage\"") {
            continue;
        }
        // The file of a running session can end in a half-written line.
        let Ok(Line {
            timestamp: Some(timestamp),
            message:
                Some(Message {
                    id: Some(id),
                    model: Some(model),
                    usage: Some(usage),
                }),
        }) = serde_json::from_str::<Line>(text)
        else {
            continue;
        };
        if model == SYNTHETIC_MODEL || since.is_some_and(|since| timestamp < since) {
            continue;
        }
        if !seen.insert(id) {
            continue;
        }
        records.push(Record {
            provider: PROVIDER,
            model,
            timestamp,
            tokens: Tokens {
                input: usage.input_tokens,
                output: usage.output_tokens,
                cache_write: usage.cache_creation_input_tokens,
                cache_read: usage.cache_read_input_tokens,
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn line(id: &str, model: &str, ts: &str, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":1,"output_tokens":{output},"cache_creation_input_tokens":10,"cache_read_input_tokens":100}}}}}}"#
        )
    }

    fn write(path: &Path, lines: &[String]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, lines.join("\n")).unwrap();
    }

    #[test]
    fn counts_each_message_once_across_files_and_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let a = line("msg_a", "claude-opus-5-5", "2026-10-21T10:00:00Z", 5);
        let b = line("msg_b", "claude-fable-5-1", "2026-10-21T11:00:00Z", 7);
        // Logged once per content block, and copied into a resumed session.
        write(
            &dir.path().join("p1/s1.jsonl"),
            &[a.clone(), a.clone(), b.clone()],
        );
        write(&dir.path().join("p2/s2.jsonl"), std::slice::from_ref(&a));
        write(
            &dir.path().join("p1/s1/subagents/agent-x.jsonl"),
            &[line("msg_c", "claude-haiku-4-5", "2026-10-21T12:00:00Z", 3)],
        );

        let mut got = records(dir.path(), None);
        got.sort_by_key(|r| r.timestamp);

        let models: Vec<_> = got.iter().map(|r| r.model.as_str()).collect();
        assert_eq!(
            models,
            ["claude-opus-5-5", "claude-fable-5-1", "claude-haiku-4-5"]
        );
        assert_eq!(
            got[0].tokens,
            Tokens {
                input: 1,
                output: 5,
                cache_write: 10,
                cache_read: 100
            }
        );
    }

    #[test]
    fn skips_synthetic_unparsable_and_unrelated_lines() {
        let dir = tempfile::tempdir().unwrap();
        let good = line("msg_a", "claude-opus-5-5", "2026-10-21T10:00:00Z", 5);
        let truncated = good[..good.len() - 20].to_string();
        write(
            &dir.path().join("p/s.jsonl"),
            &[
                r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_string(),
                line("msg_s", SYNTHETIC_MODEL, "2026-10-21T10:00:00Z", 0),
                good,
                truncated.replace("msg_a", "msg_t"),
            ],
        );
        fs::write(
            dir.path().join("p/notes.txt"),
            line("msg_n", "m", "2026-10-21T10:00:00Z", 1),
        )
        .unwrap();

        let got = records(dir.path(), None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].model, "claude-opus-5-5");
    }

    #[test]
    fn filters_by_since() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("p/s.jsonl"),
            &[
                line("msg_old", "m", "2026-10-20T23:59:59Z", 1),
                line("msg_new", "m", "2026-10-21T00:00:00Z", 2),
            ],
        );
        let since = "2026-10-21T00:00:00Z".parse().unwrap();

        let got = records(dir.path(), Some(since));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].tokens.output, 2);
    }

    #[test]
    fn missing_dir_has_no_records() {
        let dir = tempfile::tempdir().unwrap();
        assert!(records(&dir.path().join("absent"), None).is_empty());
    }

    #[test]
    fn card_without_status_line_has_todays_tokens_and_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("p/s.jsonl"),
            &[line("msg_a", "m", "2026-10-21T10:00:00Z", 1_499)],
        );
        let now = "2026-10-21T12:00:00Z".parse().unwrap();
        let today = "2026-10-21T00:00:00Z".parse().unwrap();

        let card = card(
            dir.path(),
            Some(&dir.path().join("no-limits.json")),
            today,
            now,
        )
        .unwrap();
        assert_eq!(card.as_of, None);
        assert_eq!(card.meters.len(), 1);
        assert_eq!(card.meters[0].value, "1.5K tokens");
        assert_eq!(card.meters[0].used_percent, None);
        assert!(
            card.notes[0].contains("ai-usage claude-statusline"),
            "{:?}",
            card.notes
        );
    }

    #[test]
    fn card_reports_limits_that_have_reset() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("p/s.jsonl"),
            &[line("msg_a", "m", "2026-10-21T10:00:00Z", 5)],
        );
        let today = "2026-10-21T00:00:00Z".parse().unwrap();
        let limits = dir.path().join("limits.json");
        let reported = "2026-10-21T09:00:00Z".parse().unwrap();
        let input = format!(
            r#"{{"rate_limits":{{"five_hour":{{"used_percentage":80,"resets_at":{}}}}}}}"#,
            reported_plus(reported, 3_600)
        );
        limits::statusline(&input, Some(&limits), reported);
        let now = "2026-10-21T12:00:00Z".parse().unwrap();

        let card = card(dir.path(), Some(&limits), today, now).unwrap();
        assert_eq!(card.as_of, None);
        assert_eq!(
            card.meters
                .iter()
                .filter(|m| m.used_percent.is_some())
                .count(),
            0
        );
        assert_eq!(
            card.notes,
            ["No current plan limits. Claude Code last reported them 3 h ago."]
        );
    }

    #[test]
    fn no_card_without_usage_today() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("p/s.jsonl"),
            &[line("msg_a", "m", "2026-10-20T23:00:00Z", 5)],
        );
        let now = "2026-10-21T12:00:00Z".parse().unwrap();
        let today = "2026-10-21T00:00:00Z".parse().unwrap();
        assert_eq!(card(dir.path(), None, today, now), None);
        assert_eq!(card(&dir.path().join("absent"), None, today, now), None);
    }

    fn reported_plus(at: DateTime<Utc>, seconds: i64) -> i64 {
        at.timestamp() + seconds
    }
}
