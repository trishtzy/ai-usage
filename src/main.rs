mod bar;
mod claude;
mod codex;
mod keys;
mod opencode;
mod report;
mod status;
mod usage;

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use chrono::{DateTime, Local, TimeDelta, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::report::Report;

/// Show AI plan limits and token usage from local coding-agent data.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Options for `status`, which runs when no command is given.
    #[command(flatten)]
    status: StatusArgs,

    /// Claude Code projects directory
    /// [default: $CLAUDE_CONFIG_DIR/projects, or ~/.claude/projects]
    #[arg(long, global = true, value_name = "DIR")]
    claude_dir: Option<PathBuf>,

    /// Codex sessions directory
    /// [default: $CODEX_HOME/sessions, or ~/.codex/sessions]
    #[arg(long, global = true, value_name = "DIR")]
    codex_dir: Option<PathBuf>,

    /// OpenCode data directory
    /// [default: $XDG_DATA_HOME/opencode, or ~/.local/share/opencode]
    #[arg(long, global = true, value_name = "DIR")]
    opencode_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Plan limits as progress bars, and today's tokens (the default).
    Status(StatusArgs),
    /// Token totals per model.
    Tokens(TokensArgs),
    /// A one-line footer bar, as in Rex, scrolling when it doesn't fit.
    Bar(BarArgs),
    /// Run as Claude Code's status line, saving plan limits for `status`.
    ClaudeStatusline,
}

#[derive(Args)]
struct StatusArgs {
    /// Print JSON instead of bars.
    #[arg(long)]
    json: bool,

    /// Width to lay out for [default: the terminal's, or 80]
    #[arg(long)]
    width: Option<usize>,

    /// Redraw every SECONDS inside a border, until Esc or Ctrl-C.
    #[arg(long, value_name = "SECONDS", conflicts_with = "json", value_parser = clap::value_parser!(u64).range(1..))]
    watch: Option<u64>,

    /// With --watch, the colour to paint the pane: `black` (the theme's
    /// palette black), `#rrggbb`, or `none` for see-through
    #[arg(long, value_name = "COLOR", requires = "watch", default_value = "black", value_parser = parse_background)]
    background: Background,
}

#[derive(Clone)]
struct Background(Option<String>);

fn parse_background(s: &str) -> Result<Background, String> {
    status::parse_background(s).map(Background)
}

#[derive(Args)]
struct BarArgs {
    /// Seconds between rereading usage.
    #[arg(long, value_name = "SECONDS", default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
    interval: u64,

    /// The colour to paint the bar: `black` (the theme's palette black),
    /// `#rrggbb`, or `none`
    #[arg(long, value_name = "COLOR", default_value = "black", value_parser = parse_background)]
    background: Background,
}

#[derive(Args)]
struct TokensArgs {
    /// Window to report: `today`, `all`, or a duration such as `5h` or `7d`.
    #[arg(long, default_value = "today", value_parser = parse_window)]
    since: Window,

    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
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
    let now = Local::now();
    let Some(claude_dir) = cli.claude_dir.or_else(claude::default_dir) else {
        eprintln!("ai-usage: can't find the Claude Code directory; set HOME or pass --claude-dir");
        return ExitCode::FAILURE;
    };
    let dirs = Dirs {
        claude: claude_dir,
        codex: cli.codex_dir.or_else(codex::default_dir),
        opencode: cli.opencode_dir.or_else(opencode::default_dir),
    };

    match cli.command.unwrap_or(Command::Status(cli.status)) {
        Command::Bar(args) => run_bar(&dirs, &args),
        Command::ClaudeStatusline => {
            let mut input = String::new();
            let _ = std::io::stdin().read_to_string(&mut input);
            let path = claude::limits::default_path();
            println!(
                "{}",
                claude::limits::statusline(&input, path.as_deref(), now.to_utc())
            );
        }
        Command::Status(args) => {
            if args.json {
                let cards = status_cards(&dirs, now);
                print!("{}", status::json(&cards, now.to_utc()));
                return ExitCode::SUCCESS;
            }
            let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
            let mut last_size = None;
            // Watched, Esc closes the pane. Restored when dropped.
            let keys = args.watch.and_then(|_| keys::Keys::new());
            let close_hint = if keys.is_some() { "Esc" } else { "Ctrl-C" };
            loop {
                let now = Local::now();
                let cards = status_cards(&dirs, now);
                // Measured each time, since a watched pane can be resized.
                let size = terminal_size::terminal_size();
                let style = status::Style {
                    width: args
                        .width
                        .or_else(|| size.map(|(w, _)| w.0 as usize))
                        .unwrap_or(80),
                    color,
                    background: args.watch.and(args.background.0.clone()),
                };
                let Some(seconds) = args.watch else {
                    print!("{}", status::render(&cards, now, &style));
                    break;
                };
                // Watched, the cards sit inside a border, one column in on each side.
                let inside = status::Style {
                    width: style.width.saturating_sub(2),
                    color,
                    background: style.background.clone(),
                };
                let out = status::render(&cards, now, &inside);
                let footer = format!(
                    "  {}",
                    style.paint("2", &format!("Every {seconds}s · {close_hint} closes"))
                );
                let height = size.map_or(0, |(_, h)| h.0 as usize);
                let content = style.boxed(&out, &footer, height);
                // Home the cursor and redraw over the last frame. A frame
                // that paints every cell covers the last one, as long as
                // the pane is the same size; otherwise clear first.
                let painted = style.background.is_some() && color;
                let clear = if painted && size == last_size {
                    ""
                } else {
                    "\x1b[2J"
                };
                last_size = size;
                let frame = format!("\x1b[H{clear}{}", style.fill(&content, height));
                let mut stdout = std::io::stdout().lock();
                if stdout
                    .write_all(frame.as_bytes())
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    // Nobody is reading any more, such as a closed pane.
                    break;
                }
                drop(stdout);
                // Wait out the interval, but redraw straight away if the
                // pane changes size. A new Rex pane settles on its size only
                // after the program has started.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
                let tick = std::time::Duration::from_millis(250);
                while std::time::Instant::now() < deadline && terminal_size::terminal_size() == size
                {
                    match &keys {
                        Some(keys) if keys.quit_pressed(tick) => return ExitCode::SUCCESS,
                        Some(_) => {}
                        None => std::thread::sleep(tick),
                    }
                }
            }
        }
        Command::Tokens(args) => {
            let since = args.since.start(now);
            let mut records = claude::records(&dirs.claude, since);
            if let Some(dir) = &dirs.opencode {
                records.extend(opencode::records(dir, since));
            }
            let report = Report::new(args.since.label(), since, records);
            let out = match args.format {
                Format::Table => report.table(),
                Format::Json => report.json(),
                Format::Line => report.line(),
            };
            print!("{out}");
        }
    }
    ExitCode::SUCCESS
}

/// Draws the footer bar until Esc or Ctrl-C, scrolling it when it doesn't
/// fit. The line is drawn on the bar's middle row, since its height is a
/// share of the window and changes with it.
fn run_bar(dirs: &Dirs, args: &BarArgs) {
    /// Shows the cursor again on the way out, however the bar ends.
    struct ShowCursor;
    impl Drop for ShowCursor {
        fn drop(&mut self) {
            print!("\x1b[?25h");
            let _ = std::io::stdout().flush();
        }
    }

    let keys = keys::Keys::new();
    print!("\x1b[?25l");
    let _cursor = ShowCursor;
    let interval = std::time::Duration::from_secs(args.interval);
    let mut cards = status_cards(dirs, Local::now());
    let mut read_at = std::time::Instant::now();
    let mut offset = 0;
    let mut last_size = None;
    loop {
        if read_at.elapsed() >= interval {
            cards = status_cards(dirs, Local::now());
            read_at = std::time::Instant::now();
        }
        let size = terminal_size::terminal_size();
        let (width, height) = size.map_or((80, 1), |(w, h)| (w.0 as usize, h.0 as usize));
        let cells = bar::line(&cards);
        let scrolls = bar::scrolls(&cells, width);
        // Every row is drawn in full, so a bar floating over other panes
        // hides them. Clear first only when the size changes.
        let clear = if size == last_size { "" } else { "\x1b[2J" };
        last_size = size;
        let background = args.background.0.as_deref();
        let middle = height.div_ceil(2).max(1);
        let mut frame = clear.to_string();
        for row in 1..=height.max(1) {
            let text = if row == middle {
                bar::render(&cells, width, offset, background)
            } else {
                bar::blank_row(width, background)
            };
            frame.push_str(&format!("\x1b[{row};1H{text}"));
        }
        let mut stdout = std::io::stdout().lock();
        if stdout
            .write_all(frame.as_bytes())
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return;
        }
        drop(stdout);
        let tick = if scrolls {
            offset += 1;
            std::time::Duration::from_millis(150)
        } else {
            offset = 0;
            std::time::Duration::from_millis(250)
        };
        match &keys {
            Some(keys) if keys.quit_pressed(tick) => return,
            Some(_) => {}
            None => std::thread::sleep(tick),
        }
    }
}

/// Where each provider keeps its data. Only Claude Code's is required.
struct Dirs {
    claude: PathBuf,
    codex: Option<PathBuf>,
    opencode: Option<PathBuf>,
}

/// One card per provider used today.
fn status_cards(dirs: &Dirs, now: DateTime<Local>) -> Vec<usage::Card> {
    let limits_path = claude::limits::default_path();
    let today = Window::Today.start(now).unwrap_or_else(|| now.to_utc());
    [
        claude::card(&dirs.claude, limits_path.as_deref(), today, now.to_utc()),
        dirs.codex
            .as_deref()
            .and_then(|dir| codex::card(dir, today, now.to_utc())),
        dirs.opencode
            .as_deref()
            .and_then(|dir| opencode::card(dir, today)),
    ]
    .into_iter()
    .flatten()
    .collect()
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
