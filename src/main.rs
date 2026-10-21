mod claude;
mod report;
mod usage;

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::{DateTime, Local, TimeDelta, Utc};
use clap::{Parser, ValueEnum};

use crate::report::Report;

/// Show AI token usage from local coding-agent logs.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Window to report: `today`, `all`, or a duration such as `5h` or `7d`.
    #[arg(long, default_value = "today", value_parser = parse_window)]
    since: Window,

    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,

    /// Claude Code projects directory
    /// [default: $CLAUDE_CONFIG_DIR/projects, or ~/.claude/projects]
    #[arg(long, value_name = "DIR")]
    claude_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    /// A table per model.
    Table,
    /// Totals per provider and model, for scripts.
    Json,
    /// One short line, for a Rex tab or session label.
    Line,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Window {
    Today,
    All,
    Last(String, TimeDelta),
}

impl Window {
    fn label(&self) -> String {
        match self {
            Window::Today => "today".to_string(),
            Window::All => "all".to_string(),
            Window::Last(label, _) => label.clone(),
        }
    }

    fn start(&self, now: DateTime<Local>) -> Option<DateTime<Utc>> {
        match self {
            Window::Today => now
                .date_naive()
                .and_hms_opt(0, 0, 0)?
                .and_local_timezone(Local)
                .earliest()
                .map(|t| t.to_utc()),
            Window::All => None,
            Window::Last(_, delta) => Some((now - *delta).to_utc()),
        }
    }
}

fn parse_window(s: &str) -> Result<Window, String> {
    match s {
        "today" => return Ok(Window::Today),
        "all" => return Ok(Window::All),
        _ => {}
    }
    let invalid = || format!("expected today, all, or a duration such as 5h or 7d, got {s:?}");
    let Some((split, unit)) = s.char_indices().last() else {
        return Err(invalid());
    };
    let count: i64 = s[..split].parse().map_err(|_| invalid())?;
    let delta = match unit {
        'm' => TimeDelta::try_minutes(count),
        'h' => TimeDelta::try_hours(count),
        'd' => TimeDelta::try_days(count),
        _ => None,
    };
    match delta {
        Some(delta) if count > 0 => Ok(Window::Last(s.to_string(), delta)),
        _ => Err(invalid()),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let since = cli.since.start(Local::now());

    let Some(claude_dir) = cli.claude_dir.or_else(claude::default_dir) else {
        eprintln!("ai-usage: can't find the Claude Code directory; set HOME or pass --claude-dir");
        return ExitCode::FAILURE;
    };
    let records = claude::records(&claude_dir, since);

    let report = Report::new(cli.since.label(), since, records);
    let out = match cli.format {
        Format::Table => report.table(),
        Format::Json => report.json(),
        Format::Line => report.line(),
    };
    print!("{out}");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows() {
        assert_eq!(parse_window("today"), Ok(Window::Today));
        assert_eq!(parse_window("all"), Ok(Window::All));
        assert_eq!(
            parse_window("5h"),
            Ok(Window::Last("5h".into(), TimeDelta::hours(5)))
        );
        assert_eq!(
            parse_window("7d"),
            Ok(Window::Last("7d".into(), TimeDelta::days(7)))
        );
        for bad in ["", "h", "0d", "-1d", "7w", "week", "5é"] {
            assert!(parse_window(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn today_starts_at_local_midnight() {
        let now = Local::now();
        let start = Window::Today.start(now).unwrap().with_timezone(&Local);
        assert_eq!(start.date_naive(), now.date_naive());
        assert_eq!(start.time(), chrono::NaiveTime::MIN);
    }
}
