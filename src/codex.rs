//! Reads plan limits from Codex CLI's session logs.
//!
//! Codex writes one JSONL file per session under
//! `sessions/YYYY/MM/DD/`. Each `token_count` event carries the latest
//! `rate_limits` for one limit, named by `limit_id`: the plan's own, or one
//! for a single model. Each holds up to two windows, such as five hours and
//! a week.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::usage::{Card, Meter, ago, window_label};

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

/// The latest report for each limit, with when it was made.
fn latest_limits(dir: &Path, now: DateTime<Utc>) -> BTreeMap<String, (DateTime<Utc>, RateLimits)> {
    let cutoff = SystemTime::from(now - LONGEST_WINDOW);
    let mut latest: BTreeMap<String, (DateTime<Utc>, RateLimits)> = BTreeMap::new();
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
        for line in BufReader::new(file).split(b'\n').map_while(Result::ok) {
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            if !text.contains("\"rate_limits\"") {
                continue;
            }
            let Ok(Line {
                timestamp: Some(timestamp),
                payload:
                    Some(Payload {
                        kind: Some(kind),
                        rate_limits: Some(limits),
                    }),
            }) = serde_json::from_str::<Line>(text)
            else {
                continue;
            };
            if kind != "token_count" {
                continue;
            }
            let id = limits
                .limit_id
                .clone()
                .unwrap_or_else(|| PROVIDER.to_string());
            if latest.get(&id).is_none_or(|(seen, _)| *seen < timestamp) {
                latest.insert(id, (timestamp, limits));
            }
        }
    }
    latest
}

/// The Codex card for `ai-usage status`. `None` when Codex isn't installed.
pub fn card(dir: &Path, now: DateTime<Utc>) -> Option<Card> {
    if !dir.exists() {
        return None;
    }
    let latest = latest_limits(dir, now);
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
    Some(card)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-21T12:00:00Z".parse().unwrap()
    }

    fn event(
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
                event("2026-10-21T09:00:00Z", "codex", None, 10.0, 300, 3_600),
                event("2026-10-21T11:00:00Z", "codex", None, 14.0, 300, 3_600),
            ],
        );
        write(
            dir.path(),
            "2026/10/20/b.jsonl",
            &[
                event(
                    "2026-10-21T10:00:00Z",
                    "codex_spark",
                    Some("GPT-5.3-Codex-Spark"),
                    66.0,
                    10_080,
                    86_400,
                ),
                event("2026-10-21T08:00:00Z", "codex", None, 99.0, 300, 3_600),
            ],
        );

        let card = card(dir.path(), now()).unwrap();
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
            ]
        );
        assert_eq!(card.as_of, Some("2026-10-21T11:00:00Z".parse().unwrap()));
        assert!(card.notes.is_empty());
    }

    #[test]
    fn says_so_when_every_window_has_reset() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.jsonl",
            &[event("2026-10-21T09:00:00Z", "codex", None, 56.0, 300, -60)],
        );

        let card = card(dir.path(), now()).unwrap();
        assert!(card.meters.is_empty());
        assert_eq!(
            card.notes,
            ["No current plan limits. Codex last reported them 3 h ago."]
        );
    }

    #[test]
    fn missing_dir_has_no_card() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(card(&dir.path().join("absent"), now()), None);
    }
}
