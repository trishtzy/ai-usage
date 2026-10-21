//! Reads token usage from OpenCode's SQLite databases.
//!
//! OpenCode keeps its sessions in `opencode*.db` files in its data
//! directory, one row per message with the message itself as JSON in
//! `data`. OpenCode 1 used the `message` table, with the role and `modelID`
//! in the JSON. OpenCode 2 uses `session_message`, with the role in its
//! `type` column and the model under `model.id`, and copies the old rows
//! across. An assistant message's `tokens` holds input (without cached
//! tokens), output, reasoning, and cache reads and writes. A message can be
//! in more than one table or database, so each is counted once by its id.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use crate::usage::{Card, Meter, Record, Tokens};

pub const PROVIDER: &str = "opencode";

#[derive(Deserialize)]
struct Message {
    /// OpenCode 1 only.
    role: Option<String>,
    /// OpenCode 1.
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    /// OpenCode 2.
    model: Option<Model>,
    tokens: Option<Usage>,
}

#[derive(Deserialize)]
struct Model {
    id: Option<String>,
}

/// Each version's message table, and a query for its rows since a time,
/// as id, creation time in milliseconds, JSON, and role if it's a column.
const TABLES: [(&str, &str); 2] = [
    (
        "session_message",
        "SELECT id, time_created, data, type FROM session_message WHERE time_created >= ?1",
    ),
    (
        "message",
        "SELECT id, time_created, data, NULL FROM message WHERE time_created >= ?1",
    ),
];

#[derive(Default, Deserialize)]
#[serde(default)]
struct Usage {
    input: u64,
    output: u64,
    reasoning: u64,
    cache: Cache,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Cache {
    read: u64,
    write: u64,
}

/// `$XDG_DATA_HOME/opencode`, with `XDG_DATA_HOME` defaulting to
/// `~/.local/share`.
pub fn default_dir() -> Option<PathBuf> {
    let data = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
    };
    Some(data.join("opencode"))
}

/// Returns every assistant message in the databases under `dir` created at
/// or after `since`. A missing directory yields no records.
pub fn records(dir: &Path, since: Option<DateTime<Utc>>) -> Vec<Record> {
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return records;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("opencode") && name.ends_with(".db"))
        })
        .collect();
    paths.sort();
    for path in paths {
        if let Err(err) = read_db(&path, since, &mut seen, &mut records) {
            eprintln!("ai-usage: skipping {}: {err}", path.display());
        }
    }
    records
}

fn read_db(
    path: &Path,
    since: Option<DateTime<Utc>>,
    seen: &mut HashSet<String>,
    records: &mut Vec<Record>,
) -> rusqlite::Result<()> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let since = since.map_or(i64::MIN, |since| since.timestamp_millis());
    for (table, sql) in TABLES {
        let exists: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            continue;
        }
        let mut query = conn.prepare(sql)?;
        let rows = query.query_map([since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (id, created, data, column_role) = row?;
            let Ok(message) = serde_json::from_str::<Message>(&data) else {
                continue;
            };
            // A message that failed or was cut off has no tokens.
            let Some(usage) = message.tokens else {
                continue;
            };
            let Some(timestamp) = DateTime::from_timestamp_millis(created) else {
                continue;
            };
            let role = column_role.or(message.role);
            if role.as_deref() != Some("assistant") || !seen.insert(id) {
                continue;
            }
            let model = message
                .model
                .and_then(|model| model.id)
                .or(message.model_id)
                .unwrap_or_else(|| "unknown".to_string());
            records.push(Record {
                provider: PROVIDER,
                model,
                timestamp,
                tokens: Tokens {
                    input: usage.input,
                    // Codex counts reasoning as output, so this does too.
                    output: usage.output + usage.reasoning,
                    cache_write: usage.cache.write,
                    cache_read: usage.cache.read,
                },
            });
        }
    }
    Ok(())
}

/// The OpenCode card for `ai-usage status`: tokens used since `today`.
/// OpenCode keeps no plan limits. `None` when it hasn't been used today.
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
        title: "OpenCode",
        as_of: None,
        meters: vec![Meter::today(used)],
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(path: &Path, messages: &[(&str, i64, &str)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, \
             time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL)",
        )
        .unwrap();
        for (id, created, data) in messages {
            conn.execute(
                "INSERT INTO message VALUES (?1, 's', ?2, ?2, ?3)",
                rusqlite::params![id, created, data],
            )
            .unwrap();
        }
    }

    fn assistant(model: &str, input: u64, output: u64, reasoning: u64) -> String {
        format!(
            r#"{{"role":"assistant","modelID":"{model}","tokens":{{"total":0,"input":{input},"output":{output},"reasoning":{reasoning},"cache":{{"write":3,"read":1000}}}}}}"#
        )
    }

    fn ms(s: &str) -> i64 {
        s.parse::<DateTime<Utc>>().unwrap().timestamp_millis()
    }

    #[test]
    fn reads_assistant_messages_once_across_databases() {
        let dir = tempfile::tempdir().unwrap();
        let a = assistant("deepseek", 100, 20, 5);
        let user = r#"{"role":"user"}"#.to_string();
        db(
            &dir.path().join("opencode.db"),
            &[
                ("m1", ms("2026-10-21T09:00:00Z"), &a),
                ("u1", ms("2026-10-21T09:00:00Z"), &user),
            ],
        );
        db(
            &dir.path().join("opencode-stable.db"),
            &[
                ("m1", ms("2026-10-21T09:00:00Z"), &a),
                ("m2", ms("2026-10-20T09:00:00Z"), &assistant("glm", 1, 1, 0)),
            ],
        );
        std::fs::write(dir.path().join("auth.json"), "{}").unwrap();

        let mut got = records(dir.path(), None);
        got.sort_by_key(|r| r.timestamp);
        let models: Vec<_> = got.iter().map(|r| r.model.as_str()).collect();
        assert_eq!(models, ["glm", "deepseek"]);
        assert_eq!(
            got[1].tokens,
            Tokens {
                input: 100,
                output: 25,
                cache_write: 3,
                cache_read: 1000
            }
        );

        let today = "2026-10-21T00:00:00Z".parse().unwrap();
        assert_eq!(records(dir.path(), Some(today)).len(), 1);
        let card = card(dir.path(), today).unwrap();
        assert_eq!(card.meters[0].value, "125 tokens");
    }

    #[test]
    fn reads_opencode_2_messages_and_their_copies_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        // An upgraded database: the old table, and the new one with a copy.
        db(
            &path,
            &[("m1", ms("2026-10-21T09:00:00Z"), &assistant("glm", 1, 1, 0))],
        );
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, \
             type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL, \
             time_updated INTEGER NOT NULL, data TEXT NOT NULL)",
        )
        .unwrap();
        let v2 = |model: &str, tokens: &str| {
            format!(
                r#"{{"agent":"build","model":{{"providerID":"opencode","id":"{model}"}}{tokens}}}"#
            )
        };
        let rows = [
            (
                "m1",
                "assistant",
                ms("2026-10-21T09:00:00Z"),
                v2("glm", r#","tokens":{"input":1,"output":1}"#),
            ),
            (
                "m2",
                "assistant",
                ms("2026-10-21T10:00:00Z"),
                v2(
                    "gemini",
                    r#","tokens":{"input":10,"output":5,"reasoning":1,"cache":{"read":7,"write":0}}"#,
                ),
            ),
            // Cut off before it finished, so no tokens.
            (
                "m3",
                "assistant",
                ms("2026-10-21T10:01:00Z"),
                v2("deepseek", ""),
            ),
            (
                "u1",
                "user",
                ms("2026-10-21T10:00:00Z"),
                r#"{"tokens":{"input":99}}"#.to_string(),
            ),
        ];
        for (seq, (id, kind, created, data)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO session_message VALUES (?1, 's', ?2, ?3, ?4, ?4, ?5)",
                rusqlite::params![id, kind, seq as i64, created, data],
            )
            .unwrap();
        }
        drop(conn);

        let mut got = records(dir.path(), None);
        got.sort_by_key(|r| r.timestamp);
        let models: Vec<_> = got.iter().map(|r| r.model.as_str()).collect();
        assert_eq!(models, ["glm", "gemini"]);
        assert_eq!(
            got[1].tokens,
            Tokens {
                input: 10,
                output: 6,
                cache_write: 0,
                cache_read: 7
            }
        );
    }

    #[test]
    fn no_card_without_usage_today() {
        let dir = tempfile::tempdir().unwrap();
        db(
            &dir.path().join("opencode.db"),
            &[("m1", ms("2026-10-20T09:00:00Z"), &assistant("m", 1, 1, 0))],
        );
        let today = "2026-10-21T00:00:00Z".parse().unwrap();
        assert_eq!(card(dir.path(), today), None);
        assert_eq!(card(&dir.path().join("absent"), today), None);
    }
}
