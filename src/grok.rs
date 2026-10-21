//! Reads plan usage and token usage from Grok Build, xAI's `grok` CLI.
//!
//! Grok keeps each session in `sessions/<cwd>/<session>/`. Its `usage.json`
//! is rewritten at the end of every turn, with one row per turn so far and,
//! inside each row, one per model. A row's `inputTokens` is the whole prompt,
//! cache reads and writes included, and its `outputTokens` includes
//! reasoning.
//!
//! A subagent's spend is added to its parent's ledger as well as written to
//! its own session, so subagent sessions are skipped. A fork or resume copies
//! the earlier turns into the new session unchanged, so turns are counted
//! once by when they ended, their model, and their counts.
//!
//! Grok stores its plan limit nowhere but its diagnostic log,
//! `logs/unified.jsonl`, which records each fetch of the credits config.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::usage::{Card, Meter, Record, Tokens};

pub const PROVIDER: &str = "grok";

/// The log message carrying the plan's credit usage.
const BILLING_MESSAGE: &str = "billing: fetched credits config";

#[derive(Deserialize)]
struct UsageFile {
    #[serde(default)]
    turns: Vec<Turn>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Turn {
    ended_at: Option<DateTime<Utc>>,
    primary_model_id: Option<String>,
    #[serde(flatten)]
    usage: Usage,
    #[serde(default)]
    model_usage: BTreeMap<String, Usage>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cached_read_tokens: u64,
    cache_creation_tokens: u64,
}

impl Usage {
    fn tokens(self) -> Tokens {
        Tokens {
            input: self
                .input_tokens
                .saturating_sub(self.cached_read_tokens)
                .saturating_sub(self.cache_creation_tokens),
            output: self.output_tokens,
            cache_write: self.cache_creation_tokens,
            cache_read: self.cached_read_tokens,
        }
    }
}

#[derive(Deserialize)]
struct Summary {
    session_kind: Option<String>,
}

/// `$GROK_HOME` if set, otherwise `~/.grok`.
pub fn default_dir() -> Option<PathBuf> {
    match std::env::var_os("GROK_HOME") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".grok")),
    }
}

/// Returns every model's share of every turn under `dir` that ended at or
/// after `since`. A missing directory yields no records.
pub fn records(dir: &Path, since: Option<DateTime<Utc>>) -> Vec<Record> {
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    let since_time: Option<SystemTime> = since.map(Into::into);
    for session in session_dirs(&dir.join("sessions")) {
        let path = session.join("usage.json");
        // Rewritten every turn, so an older file holds nothing newer.
        let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            continue;
        };
        if since_time.is_some_and(|since| modified < since) || is_subagent(&session) {
            continue;
        }
        let file = match std::fs::read(&path)
            .map_err(|err| err.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<UsageFile>(&bytes).map_err(|err| err.to_string())
            }) {
            Ok(file) => file,
            Err(err) => {
                eprintln!("ai-usage: skipping {}: {err}", path.display());
                continue;
            }
        };
        for turn in file.turns {
            let Some(ended_at) = turn.ended_at else {
                continue;
            };
            if since.is_some_and(|since| ended_at < since) {
                continue;
            }
            let rows = if turn.model_usage.is_empty() {
                let model = turn
                    .primary_model_id
                    .unwrap_or_else(|| "unknown".to_string());
                BTreeMap::from([(model, turn.usage)])
            } else {
                turn.model_usage
            };
            for (model, usage) in rows {
                let key = (
                    ended_at,
                    model.clone(),
                    usage.input_tokens,
                    usage.output_tokens,
                );
                if usage.input_tokens + usage.output_tokens == 0 || !seen.insert(key) {
                    continue;
                }
                records.push(Record {
                    provider: PROVIDER,
                    model,
                    timestamp: ended_at,
                    tokens: usage.tokens(),
                });
            }
        }
    }
    records
}

/// Each `sessions/<cwd>/<session>` directory.
fn session_dirs(sessions: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let Ok(projects) = std::fs::read_dir(sessions) else {
        return dirs;
    };
    for project in projects.filter_map(Result::ok) {
        let Ok(entries) = std::fs::read_dir(project.path()) else {
            continue;
        };
        dirs.extend(
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_dir()),
        );
    }
    dirs.sort();
    dirs
}

/// Whether the session is a subagent's, whose spend its parent counts.
fn is_subagent(session: &Path) -> bool {
    std::fs::read(session.join("summary.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Summary>(&bytes).ok())
        .and_then(|summary| summary.session_kind)
        .is_some_and(|kind| kind.starts_with("subagent"))
}

/// The plan's credit usage as last logged, if its period hasn't ended:
/// when it was logged, and the meter.
fn plan_limit(dir: &Path, now: DateTime<Utc>) -> Option<(DateTime<Utc>, Meter)> {
    let file = File::open(dir.join("logs/unified.jsonl")).ok()?;
    let mut latest = None;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if line.contains(BILLING_MESSAGE) {
            latest = Some(line);
        }
    }
    let entry: Value = serde_json::from_str(&latest?).ok()?;
    if entry["msg"] != BILLING_MESSAGE {
        return None;
    }
    let logged_at = entry["ts"].as_str()?.parse::<DateTime<Utc>>().ok()?;
    let config = &entry["ctx"]["config"];
    let used = config["creditUsagePercent"].as_f64()?;
    let period = &config["currentPeriod"];
    let ends = period["end"].as_str()?.parse::<DateTime<Utc>>().ok()?;
    if ends <= now {
        return None;
    }
    let label = match period["type"].as_str() {
        Some(kind) if kind.ends_with("WEEKLY") => "This week",
        Some(kind) if kind.ends_with("MONTHLY") => "This month",
        _ => "Credits",
    };
    Some((logged_at, Meter::limit(label.to_string(), used, Some(ends))))
}

/// The Grok card for `ai-usage status`: the plan's credit usage and tokens
/// used since `today`. `None` when Grok hasn't been used today.
pub fn card(dir: &Path, today: DateTime<Utc>, now: DateTime<Utc>) -> Option<Card> {
    let used: u64 = records(dir, Some(today))
        .iter()
        .map(|r| r.tokens.input + r.tokens.output)
        .sum();
    if used == 0 {
        return None;
    }
    let mut card = Card {
        provider: PROVIDER,
        title: "Grok",
        as_of: None,
        meters: Vec::new(),
        notes: Vec::new(),
    };
    if let Some((logged_at, meter)) = plan_limit(dir, now) {
        card.as_of = Some(logged_at);
        card.meters.push(meter);
    }
    card.meters.push(Meter::today(used));
    Some(card)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-21T12:00:00Z".parse().unwrap()
    }

    fn today() -> DateTime<Utc> {
        "2026-10-21T00:00:00Z".parse().unwrap()
    }

    /// A turn as Grok writes it, with one model's share.
    fn turn(n: u32, ended_at: &str, model: &str, input: u64, cached: u64, output: u64) -> String {
        let usage = format!(
            r#""inputTokens":{input},"outputTokens":{output},"cachedReadTokens":{cached},"cacheCreationTokens":0,"reasoningTokens":1,"totalTokens":{},"modelCalls":1"#,
            input + output
        );
        format!(
            r#"{{"turnNumber":{n},"endedAt":"{ended_at}",{usage},"turnCount":1,"primaryModelId":"{model}","modelUsage":{{"{model}":{{{usage}}}}}}}"#
        )
    }

    fn session(dir: &Path, cwd: &str, id: &str, kind: Option<&str>, turns: &[String]) {
        let path = dir.join("sessions").join(cwd).join(id);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("usage.json"),
            format!(
                r#"{{"sessionId":"{id}","updatedAt":"2026-10-21T10:00:00Z","session":{{}},"turns":[{}]}}"#,
                turns.join(",")
            ),
        )
        .unwrap();
        let kind = kind.map_or("null".to_string(), |k| format!("{k:?}"));
        fs::write(
            path.join("summary.json"),
            format!(r#"{{"session_id":"{id}","session_kind":{kind}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn counts_turns_once_and_skips_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let first = turn(
            1,
            "2026-10-20T09:00:00.123456789+00:00",
            "grok-4.6",
            1000,
            800,
            50,
        );
        let second = turn(2, "2026-10-21T09:00:00+00:00", "grok-4.6", 500, 100, 20);
        session(
            dir.path(),
            "%2Fsrc%2Fapp",
            "s1",
            None,
            std::slice::from_ref(&first),
        );
        // Resumed: the first turn is copied across.
        session(
            dir.path(),
            "%2Fsrc%2Fapp",
            "s2",
            Some("fork"),
            &[first, second],
        );
        session(
            dir.path(),
            "%2Fsrc%2Fapp",
            "s3",
            Some("subagent"),
            &[turn(1, "2026-10-21T09:30:00+00:00", "grok-4.6", 9, 0, 9)],
        );

        let mut got = records(dir.path(), None);
        got.sort_by_key(|r| r.timestamp);
        let tokens: Vec<_> = got.iter().map(|r| r.tokens).collect();
        assert_eq!(
            tokens,
            [
                Tokens {
                    input: 200,
                    output: 50,
                    cache_write: 0,
                    cache_read: 800
                },
                Tokens {
                    input: 400,
                    output: 20,
                    cache_write: 0,
                    cache_read: 100
                },
            ]
        );
        assert_eq!(got[0].model, "grok-4.6");

        let card = card(dir.path(), today(), now()).unwrap();
        assert_eq!(card.meters.len(), 1);
        assert_eq!(card.meters[0].value, "420 tokens");
    }

    #[test]
    fn shows_the_plan_limit_until_its_period_ends() {
        let dir = tempfile::tempdir().unwrap();
        session(
            dir.path(),
            "p",
            "s1",
            None,
            &[turn(1, "2026-10-21T09:00:00+00:00", "grok-4.6", 10, 0, 5)],
        );
        let billing = |ts: &str, used: f64, end: &str| {
            format!(
                r#"{{"ts":"{ts}","src":"shell","lvl":"info","msg":"{BILLING_MESSAGE}","ctx":{{"config":{{"creditUsagePercent":{used},"currentPeriod":{{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-10-19T00:00:00Z","end":"{end}"}}}},"subscriptionTier":"SuperGrok"}}}}"#
            )
        };
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        let log = [
            billing("2026-10-21T08:00:00Z", 10.0, "2026-10-26T00:00:00Z"),
            r#"{"ts":"2026-10-21T08:30:00Z","msg":"shell.turn.inference_done"}"#.to_string(),
            billing("2026-10-21T09:00:00Z", 12.5, "2026-10-26T00:00:00Z"),
        ];
        fs::write(dir.path().join("logs/unified.jsonl"), log.join("\n")).unwrap();

        let card = card(dir.path(), today(), now()).unwrap();
        assert_eq!(card.meters.len(), 2);
        assert_eq!(card.meters[0].label, "This week");
        assert_eq!(card.meters[0].used_percent, Some(12.5));
        assert_eq!(card.as_of, Some("2026-10-21T09:00:00Z".parse().unwrap()));

        // Once the period has ended, the limit is stale.
        let later = "2026-10-27T00:00:00Z".parse().unwrap();
        assert_eq!(plan_limit(dir.path(), later), None);
    }

    #[test]
    fn no_card_without_usage_today() {
        let dir = tempfile::tempdir().unwrap();
        session(
            dir.path(),
            "p",
            "s1",
            None,
            &[turn(1, "2026-10-20T09:00:00+00:00", "grok-4.6", 10, 0, 5)],
        );
        assert_eq!(card(dir.path(), today(), now()), None);
        assert_eq!(card(&dir.path().join("absent"), today(), now()), None);
    }
}
