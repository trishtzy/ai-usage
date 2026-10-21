//! Totals records per model and renders them.

use std::collections::BTreeMap;

use chrono::{DateTime, Local, Utc};
use serde_json::json;

use crate::usage::{Record, Tokens};

#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub provider: &'static str,
    pub model: String,
    pub messages: u64,
    pub tokens: Tokens,
}

#[derive(Debug)]
pub struct Report {
    /// How the window was asked for, such as `today` or `7d`.
    pub window: String,
    pub since: Option<DateTime<Utc>>,
    /// One row per provider and model, busiest first within each provider.
    pub rows: Vec<Row>,
}

impl Report {
    pub fn new(window: String, since: Option<DateTime<Utc>>, records: Vec<Record>) -> Self {
        let mut totals: BTreeMap<(&'static str, String), (u64, Tokens)> = BTreeMap::new();
        for record in records {
            let (messages, tokens) = totals.entry((record.provider, record.model)).or_default();
            *messages += 1;
            *tokens += record.tokens;
        }
        let mut rows: Vec<Row> = totals
            .into_iter()
            .map(|((provider, model), (messages, tokens))| Row {
                provider,
                model,
                messages,
                tokens,
            })
            .collect();
        rows.sort_by(|a, b| {
            a.provider
                .cmp(b.provider)
                .then(b.tokens.total().cmp(&a.tokens.total()))
        });
        Report {
            window,
            since,
            rows,
        }
    }

    /// Totals per provider, in provider order.
    fn providers(&self) -> Vec<(&'static str, Tokens)> {
        let mut totals: Vec<(&'static str, Tokens)> = Vec::new();
        for row in &self.rows {
            match totals.last_mut() {
                Some((provider, tokens)) if *provider == row.provider => *tokens += row.tokens,
                _ => totals.push((row.provider, row.tokens)),
            }
        }
        totals
    }

    fn heading(&self) -> String {
        match self.since {
            Some(since) => format!(
                "AI usage · {} (since {})",
                self.window,
                since.with_timezone(&Local).format("%Y-%m-%d %H:%M")
            ),
            None => "AI usage · all time".to_string(),
        }
    }

    pub fn table(&self) -> String {
        if self.rows.is_empty() {
            return format!("{}\nNo usage.\n", self.heading());
        }
        let header = [
            "PROVIDER",
            "MODEL",
            "MSGS",
            "INPUT",
            "OUTPUT",
            "CACHE WRITE",
            "CACHE READ",
            "TOTAL",
        ];
        let cells = |provider: &str, model: &str, messages: u64, t: &Tokens| {
            [
                provider.to_string(),
                model.to_string(),
                thousands(messages),
                thousands(t.input),
                thousands(t.output),
                thousands(t.cache_write),
                thousands(t.cache_read),
                thousands(t.total()),
            ]
        };
        let mut lines: Vec<[String; 8]> = vec![header.map(String::from)];
        let mut sum = Tokens::default();
        let mut messages = 0;
        for row in &self.rows {
            lines.push(cells(row.provider, &row.model, row.messages, &row.tokens));
            sum += row.tokens;
            messages += row.messages;
        }
        lines.push(cells("", "TOTAL", messages, &sum));

        let mut widths = [0; 8];
        for line in &lines {
            for (width, cell) in widths.iter_mut().zip(line) {
                *width = (*width).max(cell.chars().count());
            }
        }
        let mut out = format!("{}\n", self.heading());
        for line in &lines {
            let mut text = String::new();
            for (i, (cell, width)) in line.iter().zip(widths).enumerate() {
                if i > 0 {
                    text.push_str("  ");
                }
                // Names line up on the left, numbers on the right.
                if i < 2 {
                    text.push_str(&format!("{cell:<width$}"));
                } else {
                    text.push_str(&format!("{cell:>width$}"));
                }
            }
            out.push_str(text.trim_end());
            out.push('\n');
        }
        out
    }

    pub fn json(&self) -> String {
        let tokens = |t: &Tokens| {
            json!({
                "input": t.input,
                "output": t.output,
                "cache_write": t.cache_write,
                "cache_read": t.cache_read,
                "total": t.total(),
            })
        };
        let models: Vec<_> = self
            .rows
            .iter()
            .map(|row| {
                json!({
                    "provider": row.provider,
                    "model": row.model,
                    "messages": row.messages,
                    "tokens": tokens(&row.tokens),
                })
            })
            .collect();
        let providers: Vec<_> = self
            .providers()
            .iter()
            .map(|(provider, t)| json!({ "provider": provider, "tokens": tokens(t) }))
            .collect();
        let value = json!({
            "window": self.window,
            "since": self.since.map(|s| s.to_rfc3339()),
            "providers": providers,
            "models": models,
        });
        format!("{value:#}\n")
    }

    /// One short line for a tab or session label, such as `claude 12.3M`.
    /// It counts input and output only: cache reads re-read the whole
    /// conversation every turn and would swamp the number.
    pub fn line(&self) -> String {
        let providers = self.providers();
        if providers.is_empty() {
            return "no AI usage\n".to_string();
        }
        let parts: Vec<_> = providers
            .iter()
            .map(|(provider, t)| format!("{provider} {}", compact(t.input + t.output)))
            .collect();
        format!("{}\n", parts.join(" · "))
    }
}

/// 1234567 → `1,234,567`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// 1234567 → `1.2M`.
fn compact(n: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "K")];
    for (size, unit) in UNITS {
        if n >= size {
            return format!("{:.1}{unit}", n as f64 / size as f64);
        }
    }
    n.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(provider: &'static str, model: &str, output: u64) -> Record {
        Record {
            provider,
            model: model.to_string(),
            timestamp: "2026-10-21T10:00:00Z".parse().unwrap(),
            tokens: Tokens {
                input: 1,
                output,
                cache_write: 0,
                cache_read: 0,
            },
        }
    }

    #[test]
    fn totals_per_model_busiest_first() {
        let report = Report::new(
            "all".into(),
            None,
            vec![
                record("claude", "small", 1),
                record("claude", "big", 50),
                record("claude", "small", 2),
            ],
        );
        let rows: Vec<_> = report
            .rows
            .iter()
            .map(|r| (r.model.as_str(), r.messages, r.tokens.total()))
            .collect();
        assert_eq!(rows, [("big", 1, 51), ("small", 2, 5)]);
        assert_eq!(report.line(), "claude 56\n");
    }

    #[test]
    fn line_joins_providers() {
        let report = Report::new(
            "all".into(),
            None,
            vec![
                record("claude", "a", 1_499_999),
                record("codex", "b", 2_000),
            ],
        );
        assert_eq!(report.line(), "claude 1.5M · codex 2.0K\n");
    }

    #[test]
    fn line_leaves_out_cache() {
        let mut cached = record("claude", "a", 99);
        cached.tokens.cache_write = 5_000;
        cached.tokens.cache_read = 1_000_000;
        let report = Report::new("all".into(), None, vec![cached]);
        assert_eq!(report.line(), "claude 100\n");
    }

    #[test]
    fn empty_report() {
        let report = Report::new("all".into(), None, vec![]);
        assert_eq!(report.line(), "no AI usage\n");
        assert_eq!(report.table(), "AI usage · all time\nNo usage.\n");
    }

    #[test]
    fn formats_numbers() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(12_345), "12.3K");
        assert_eq!(compact(3_000_000_000), "3.0B");
    }
}
