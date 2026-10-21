//! Reads plan limits and token usage from Codex CLI's session logs.
//!
//! Codex writes one JSONL file per session under
//! `sessions/YYYY/MM/DD/`. Each `token_count` event carries the latest
//! `rate_limits` for one limit, named by `limit_id`: the plan's own, or one
//! for a single model. Each holds up to two windows, such as five hours and
//! a week. The event also carries the session's running token total, so a
//! session's usage over a span is the change in that total across it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::usage::{Card, Meter, Tokens, ago, window_label};

pub const PROVIDER: &str = "codex";

/// The longest window Codex reports. Files older than this can't hold a
/// window that hasn't reset.
const LONGEST_WINDOW: TimeDelta = TimeDelta::days(30);

#[derive(Deserialize)]
struct Line {
    timestamp: Option<DateTime<Utc>>,
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    rate_limits: Option<RateLimits>,
    info: Option<Info>,
}

#[derive(Deserialize)]
struct Info {
    total_token_usage: Option<Usage>,
}

/// A session's running total. `input_tokens` includes the cached ones,
/// and `output_tokens` includes reasoning.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
}

impl Usage {
    /// What was used between `self` and the later total `then`.
    fn until(self, then: Usage) -> Tokens {
        let cached = then
            .cached_input_tokens
            .saturating_sub(self.cached_input_tokens);
        let input = then.input_tokens.saturating_sub(self.input_tokens);
        Tokens {
            input: input.saturating_sub(cached),
            output: then.output_tokens.saturating_sub(self.output_tokens),
            cache_write: 0,
            cache_read: cached,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RateLimits {
    limit_id: Option<String>,
    limit_name: Option<String>,
    primary: Option<Window>,
    secondary: Option<Window>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct Window {
    used_percent: f64,
    window_minutes: Option<u64>,
    /// Unix seconds.
    resets_at: Option<i64>,
}

/// `$CODEX_HOME/sessions`, with `CODEX_HOME` defaulting to `~/.codex`.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(dir).join("sessions"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex/sessions"))
}

#[derive(Default)]
struct Scan {
    /// The latest report for each limit, with when it was made.
    limits: BTreeMap<String, (DateTime<Utc>, RateLimits)>,
    /// Tokens used since `since`, across every session.
    used: Tokens,
}

/// Reads every session written to in the last 30 days.
fn scan(dir: &Path, since: DateTime<Utc>, now: DateTime<Utc>) -> Scan {
    let cutoff = SystemTime::from((now - LONGEST_WINDOW).min(since));
    let mut scan = Scan::default();
    for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
        if modified.is_some_and(|m| m < cutoff) {
            continue;
        }
        let Ok(file) = File::open(path) else { continue };
        // The session's total as of `since`, and its latest total after.
        let (mut before, mut after) = (Usage::default(), None);
        for line in BufReader::new(file).split(b'\n').map_while(Result::ok) {
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            if !text.contains("\"token_count\"") {
                continue;
            }
            let Ok(Line {
                timestamp: Some(timestamp),
                payload: Some(payload),
            }) = serde_json::from_str::<Line>(text)
            else {
                continue;
            };
            if payload.kind.as_deref() != Some("token_count") {
                continue;
            }
            if let Some(total) = payload.info.and_then(|info| info.total_token_usage) {
                if timestamp < since {
                    before = total;
                } else {
                    after = Some(total);
                }
            }
            if let Some(limits) = payload.rate_limits {
                let id = limits
                    .limit_id
                    .clone()
                    .unwrap_or_else(|| PROVIDER.to_string());
                if scan
                    .limits
                    .get(&id)
                    .is_none_or(|(seen, _)| *seen < timestamp)
                {
                    scan.limits.insert(id, (timestamp, limits));
                }
            }
        }
        if let Some(after) = after {
            scan.used += before.until(after);
        }
    }
    scan
}

/// The Codex card for `ai-usage status`: plan limits and tokens used since
/// `today`. `None` when no session has used any since then.
pub fn card(dir: &Path, today: DateTime<Utc>, now: DateTime<Utc>) -> Option<Card> {
    if !dir.exists() {
        return None;
    }
    let Scan {
        limits: latest,
        used,
    } = scan(dir, today, now);
    if used.input + used.output == 0 {
        return None;
    }
    let mut card = Card {
        provider: PROVIDER,
        title: "Codex",
        as_of: None,
        meters: Vec::new(),
        notes: Vec::new(),
    };
    // The plan's own limit first, then per-model ones by name.
    let mut limits: Vec<_> = latest.into_values().collect();
    limits.sort_by(|(_, a), (_, b)| {
        (a.limit_name.is_some(), &a.limit_name).cmp(&(b.limit_name.is_some(), &b.limit_name))
    });
    let mut last_report = None;
    for (as_of, limit) in &limits {
        last_report = last_report.max(Some(*as_of));
        for window in [limit.primary, limit.secondary].into_iter().flatten() {
            let resets_at = window
                .resets_at
                .and_then(|t| DateTime::from_timestamp(t, 0));
            if resets_at.is_none_or(|t| t <= now) {
                continue;
            }
            let when = window
                .window_minutes
                .map(window_label)
                .unwrap_or_else(|| "Limit".to_string());
            let meter = match &limit.limit_name {
                Some(name) => Meter {
                    detail: Some(when),
                    ..Meter::limit(name.clone(), window.used_percent, resets_at)
                },
                None => Meter::limit(when, window.used_percent, resets_at),
            };
            card.meters.push(meter);
            card.as_of = card.as_of.max(Some(*as_of));
        }
    }
    if card.meters.is_empty() {
        card.notes.push(match last_report {
            Some(at) => format!(
                "No current plan limits. Codex last reported them {}.",
                ago(at, now)
            ),
            None => "No plan limits reported in the last 30 days.".to_string(),
        });
    }
    card.meters.push(Meter::today(used.input + used.output));
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

    fn limit(
        ts: &str,
        id: &str,
        name: Option<&str>,
        used: f64,
        minutes: u64,
        resets_in: i64,
    ) -> String {
        let name = name.map_or("null".to_string(), |n| format!("{n:?}"));
        let resets_at = now().timestamp() + resets_in;
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"{id}","limit_name":{name},"primary":{{"used_percent":{used},"window_minutes":{minutes},"resets_at":{resets_at}}},"secondary":null}}}}}}"#
        )
    }

    /// A session's running total: all input, the cached part of it, and output.
    fn total(ts: &str, input: u64, cached: u64, output: u64) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}}}},"rate_limits":null}}}}"#
        )
    }

    fn write(dir: &Path, name: &str, lines: &[String]) {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, lines.join("\n")).unwrap();
    }

    #[test]
    fn shows_latest_report_per_limit_plan_first() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "2026/10/21/a.jsonl",
            &[
                limit("2026-10-21T09:00:00Z", "codex", None, 10.0, 300, 3_600),
                limit("2026-10-21T11:00:00Z", "codex", None, 14.0, 300, 3_600),
                total("2026-10-21T11:00:00Z", 1_000, 0, 500),
            ],
        );
        write(
            dir.path(),
            "2026/10/20/b.jsonl",
            &[
                limit(
                    "2026-10-21T10:00:00Z",
                    "codex_spark",
                    Some("GPT-5.3-Codex-Spark"),
                    66.0,
                    10_080,
                    86_400,
                ),
                limit("2026-10-21T08:00:00Z", "codex", None, 99.0, 300, 3_600),
            ],
        );

        let card = card(dir.path(), today(), now()).unwrap();
        let got: Vec<_> = card
            .meters
            .iter()
            .map(|m| (m.label.as_str(), m.detail.as_deref(), m.value.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("Current session", None, "14% used"),
                ("GPT-5.3-Codex-Spark", Some("This week"), "66% used"),
                ("Today", None, "1.5K tokens"),
            ]
        );
        assert_eq!(card.as_of, Some("2026-10-21T11:00:00Z".parse().unwrap()));
        assert!(card.notes.is_empty());
    }

    #[test]
    fn counts_only_what_sessions_used_since_today() {
        let dir = tempfile::tempdir().unwrap();
        // A session that ran across midnight, then one that started today.
        write(
            dir.path(),
            "a.jsonl",
            &[
                total("2026-10-20T23:00:00Z", 10_000, 6_000, 300),
                total("2026-10-21T01:00:00Z", 15_000, 9_000, 500),
                total("2026-10-21T02:00:00Z", 16_000, 9_500, 600),
            ],
        );
        write(
            dir.path(),
            "b.jsonl",
            &[total("2026-10-21T03:00:00Z", 400, 100, 50)],
        );

        let used = scan(dir.path(), today(), now()).used;
        // a: 6,000 more input, 3,500 of it cached, and 300 more output.
        assert_eq!(
            used,
            Tokens {
                input: 2_500 + 300,
                output: 300 + 50,
                cache_write: 0,
                cache_read: 3_500 + 100
            }
        );
    }

    #[test]
    fn says_so_when_every_window_has_reset() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.jsonl",
            &[
                limit("2026-10-21T09:00:00Z", "codex", None, 56.0, 300, -60),
                total("2026-10-21T09:00:00Z", 10, 0, 1),
            ],
        );

        let card = card(dir.path(), today(), now()).unwrap();
        assert_eq!(card.meters.len(), 1);
        assert_eq!(
            card.notes,
            ["No current plan limits. Codex last reported them 3 h ago."]
        );
    }

    #[test]
    fn no_card_without_usage_today() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.jsonl",
            &[
                limit("2026-10-21T09:00:00Z", "codex", None, 14.0, 300, 3_600),
                total("2026-10-20T22:00:00Z", 10, 0, 1),
            ],
        );
        assert_eq!(card(dir.path(), today(), now()), None);
        assert_eq!(card(&dir.path().join("absent"), today(), now()), None);
    }
}
