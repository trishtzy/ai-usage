//! Reads token usage from Cursor's chat store.
//!
//! Cursor keeps its chats in `state.vscdb`, a SQLite file in its global
//! storage, as JSON values in the `cursorDiskKV` table. Each conversation is
//! a `composerData:<conversation>` row listing its messages in order under
//! `fullConversationHeadersOnly`, and each message a
//! `bubbleId:<conversation>:<message>` row. A reply that finished carries
//! `tokenCount`, with input and output; Cursor records no cache split.
//!
//! A message names its model in `modelInfo` only when the model was chosen,
//! usually on the prompt that starts a turn, so each reply takes the last
//! model named before it, or else the conversation's current model. Newer
//! messages have a `createdAt`. Older ones don't, and take the time the
//! conversation started.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};

use crate::usage::{Card, Meter, Record, Tokens};

pub const PROVIDER: &str = "cursor";

/// Every message of every conversation, in order, with the fields needed.
const MESSAGES: &str = "\
SELECT c.key,
       json_extract(c.value, '$.createdAt'),
       json_extract(c.value, '$.modelConfig.modelName'),
       json_extract(b.value, '$.createdAt'),
       json_extract(b.value, '$.modelInfo.modelName'),
       json_extract(b.value, '$.tokenCount.inputTokens'),
       json_extract(b.value, '$.tokenCount.outputTokens')
FROM cursorDiskKV c, json_each(c.value, '$.fullConversationHeadersOnly') h
JOIN cursorDiskKV b ON b.key = 'bubbleId:' || json_extract(c.value, '$.composerId')
                               || ':' || json_extract(h.value, '$.bubbleId')
WHERE c.key LIKE 'composerData:%'
ORDER BY c.key, h.key";

/// `Cursor/User/globalStorage/state.vscdb` in the app support directory:
/// `~/Library/Application Support` on macOS, otherwise `$XDG_CONFIG_HOME`,
/// defaulting to `~/.config`.
pub fn default_path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let support = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from)
    };
    Some(support.join("Cursor/User/globalStorage/state.vscdb"))
}

/// Returns every reply in the store at `path` made at or after `since`. A
/// missing store means Cursor isn't installed and yields no records.
pub fn records(path: &Path, since: Option<DateTime<Utc>>) -> Vec<Record> {
    if let Some(since) = since
        && !written_since(path, since.into())
    {
        return Vec::new();
    }
    match read(path, since) {
        Ok(records) => records,
        Err(err) => {
            eprintln!("ai-usage: skipping {}: {err}", path.display());
            Vec::new()
        }
    }
}

/// Whether the store, or its write-ahead log, was written at or after
/// `since`. A store that doesn't exist wasn't.
fn written_since(path: &Path, since: SystemTime) -> bool {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    [path, Path::new(&wal)].iter().any(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .is_ok_and(|modified| modified >= since)
    })
}

fn read(path: &Path, since: Option<DateTime<Utc>>) -> rusqlite::Result<Vec<Record>> {
    let mut records = Vec::new();
    if !path.exists() {
        return Ok(records);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut query = conn.prepare(MESSAGES)?;
    let mut rows = query.query([])?;
    let mut conversation = String::new();
    let mut model: Option<String> = None;
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        if key != conversation {
            conversation = key;
            model = None;
        }
        if let Some(named) = row.get::<_, Option<String>>(4)?.filter(|m| !m.is_empty()) {
            model = Some(named);
        }
        let input = row.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as u64;
        let output = row.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u64;
        if input + output == 0 {
            continue;
        }
        let timestamp = match row.get::<_, Option<String>>(3)? {
            Some(at) => at.parse::<DateTime<Utc>>().ok(),
            None => row
                .get::<_, Option<i64>>(1)?
                .and_then(DateTime::from_timestamp_millis),
        };
        let Some(timestamp) = timestamp else {
            continue;
        };
        if since.is_some_and(|since| timestamp < since) {
            continue;
        }
        let model = model
            .clone()
            .or(row.get::<_, Option<String>>(2)?)
            .unwrap_or_else(|| "unknown".to_string());
        records.push(Record {
            provider: PROVIDER,
            model,
            timestamp,
            tokens: Tokens {
                input,
                output,
                ..Tokens::default()
            },
        });
    }
    Ok(records)
}

/// The Cursor card for `ai-usage status`: tokens used since `today`. The
/// store keeps no plan limits. `None` when Cursor hasn't been used today.
pub fn card(path: &Path, today: DateTime<Utc>) -> Option<Card> {
    let used: u64 = records(path, Some(today))
        .iter()
        .map(|r| r.tokens.input + r.tokens.output)
        .sum();
    if used == 0 {
        return None;
    }
    Some(Card {
        provider: PROVIDER,
        title: "Cursor",
        as_of: None,
        meters: vec![Meter::today(used)],
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store holding one conversation, `c1`, of `messages`: id, then the
    /// message's JSON.
    fn store(path: &Path, messages: &[(&str, String)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
        )
        .unwrap();
        let headers: Vec<String> = messages
            .iter()
            .map(|(id, _)| format!(r#"{{"bubbleId":"{id}","type":2}}"#))
            .collect();
        let composer = format!(
            r#"{{"composerId":"c1","createdAt":{},"modelConfig":{{"modelName":"default"}},"fullConversationHeadersOnly":[{}]}}"#,
            ms("2026-10-21T08:00:00Z"),
            headers.join(",")
        );
        conn.execute(
            "INSERT INTO cursorDiskKV VALUES ('composerData:c1', ?1)",
            [composer],
        )
        .unwrap();
        for (id, json) in messages {
            conn.execute(
                "INSERT INTO cursorDiskKV VALUES (?1, ?2)",
                [format!("bubbleId:c1:{id}"), json.clone()],
            )
            .unwrap();
        }
    }

    fn ms(s: &str) -> i64 {
        s.parse::<DateTime<Utc>>().unwrap().timestamp_millis()
    }

    fn message(at: Option<&str>, model: Option<&str>, input: u64, output: u64) -> String {
        let mut fields = vec![format!(
            r#""tokenCount":{{"inputTokens":{input},"outputTokens":{output}}}"#
        )];
        if let Some(at) = at {
            fields.push(format!(r#""createdAt":"{at}""#));
        }
        if let Some(model) = model {
            fields.push(format!(r#""modelInfo":{{"modelName":"{model}"}}"#));
        }
        format!("{{{}}}", fields.join(","))
    }

    #[test]
    fn reads_replies_with_the_last_model_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.vscdb");
        store(
            &path,
            &[
                // Older, with no time: takes the conversation's start.
                ("a", message(None, None, 100, 10)),
                (
                    "b",
                    message(Some("2026-10-21T09:00:00Z"), Some("sonnet"), 0, 0),
                ),
                (
                    "c",
                    message(Some("2026-10-21T09:00:05Z"), Some(""), 200, 20),
                ),
                (
                    "d",
                    message(Some("2026-10-21T10:00:00Z"), Some("gpt-5"), 0, 0),
                ),
                ("e", message(Some("2026-10-21T10:00:09Z"), None, 300, 30)),
            ],
        );

        let got = records(&path, None);
        let summary: Vec<_> = got
            .iter()
            .map(|r| (r.model.as_str(), r.tokens.input, r.tokens.output))
            .collect();
        assert_eq!(
            summary,
            [
                ("default", 100, 10),
                ("sonnet", 200, 20),
                ("gpt-5", 300, 30)
            ]
        );
        assert_eq!(
            got[0].timestamp,
            "2026-10-21T08:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );

        let since = "2026-10-21T09:30:00Z".parse().unwrap();
        // The file was just written, so it isn't skipped as stale.
        assert_eq!(records(&path, Some(since)).len(), 1);
    }

    #[test]
    fn card_counts_today_and_skips_a_missing_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.vscdb");
        store(
            &path,
            &[
                ("a", message(Some("2026-10-20T23:00:00Z"), Some("m"), 5, 5)),
                ("b", message(Some("2026-10-21T01:00:00Z"), None, 40, 2)),
            ],
        );
        let today = "2026-10-21T00:00:00Z".parse().unwrap();
        assert_eq!(card(&path, today).unwrap().meters[0].value, "42 tokens");
        assert_eq!(card(&dir.path().join("absent.vscdb"), today), None);
    }
}
