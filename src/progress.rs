use std::env;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(80);
const DEFAULT_WIDTH: usize = 80;

const CLEAR_LINE: &str = "\r\x1b[2K";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Animated status line on stderr. Does nothing when stderr is not a terminal,
/// so piped output and cron logs stay clean.
pub struct Spinner {
    active: Option<Active>,
}

struct Active {
    line: Arc<Mutex<Line>>,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    started: Instant,
    style: Style,
}

#[derive(Default)]
struct Line {
    status: String,
    /// Shown dimmed after the status and truncated from the left to fit.
    detail: String,
}

#[derive(Clone, Copy)]
struct Style {
    color: bool,
    width: usize,
}

impl Spinner {
    pub fn start(status: &str) -> Self {
        if !io::stderr().is_terminal() || !supports_ansi() {
            return Self { active: None };
        }

        let style = Style {
            color: env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()),
            width: terminal_width(),
        };
        let line = Arc::new(Mutex::new(Line {
            status: status.to_string(),
            detail: String::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));

        let handle = {
            let line = Arc::clone(&line);
            let stop = Arc::clone(&stop);
            thread::spawn(move || animate(&line, &stop, style))
        };

        Self {
            active: Some(Active {
                line,
                stop,
                handle,
                started: Instant::now(),
                style,
            }),
        }
    }

    /// Replaces the status line. The closure only runs while the spinner is
    /// visible, so callers don't pay for formatting otherwise.
    pub fn update(&self, line: impl FnOnce() -> (String, String)) {
        let Some(active) = &self.active else {
            return;
        };

        let (status, detail) = line();
        if let Ok(mut current) = active.line.lock() {
            current.status = status;
            current.detail = detail;
        }
    }

    /// Stops the animation and leaves a `✔ <summary> (<elapsed>)` line behind.
    pub fn finish(mut self, summary: &str) {
        let Some(active) = self.active.take() else {
            return;
        };

        let elapsed = active.started.elapsed();
        let style = active.style;
        active.stop();

        let (check, dim, reset) = if style.color {
            (GREEN, DIM, RESET)
        } else {
            ("", "", "")
        };
        let mut stderr = io::stderr();
        let _ = writeln!(
            stderr,
            "{check}✔{reset} {summary} {dim}({:.1}s){reset}",
            elapsed.as_secs_f64()
        );
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.stop();
        }
    }
}

impl Active {
    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.thread().unpark();
        let _ = self.handle.join();

        let mut stderr = io::stderr();
        let _ = write!(stderr, "{CLEAR_LINE}");
        let _ = stderr.flush();
    }
}

/// The spinner redraws itself with ANSI escapes. Windows consoles only honor
/// them once virtual terminal processing is enabled, which this call attempts.
#[cfg(windows)]
fn supports_ansi() -> bool {
    ratatui::crossterm::ansi_support::supports_ansi()
}

#[cfg(not(windows))]
fn supports_ansi() -> bool {
    true
}

fn terminal_width() -> usize {
    ratatui::crossterm::terminal::size()
        .ok()
        .map(|(columns, _)| usize::from(columns))
        .filter(|columns| *columns > 0)
        .or_else(|| env::var("COLUMNS").ok()?.parse().ok())
        .unwrap_or(DEFAULT_WIDTH)
}

fn animate(line: &Mutex<Line>, stop: &AtomicBool, style: Style) {
    let mut stderr = io::stderr();

    for frame in FRAMES.iter().cycle() {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        if let Ok(line) = line.lock() {
            let _ = write!(stderr, "{CLEAR_LINE}{}", render(frame, &line, style));
            let _ = stderr.flush();
        }

        thread::park_timeout(TICK);
    }
}

fn render(frame: &str, line: &Line, style: Style) -> String {
    // Keep one spare column: writing into the last one wraps on some terminals,
    // and a wrapped line can no longer be cleared with `\r`.
    let available = style.width.saturating_sub(1);
    let status = truncate_end(&line.status, available.saturating_sub(2));
    let room = available.saturating_sub(2 + status.chars().count() + 2);
    let detail = truncate_start(&line.detail, room);

    let (cyan, dim, reset) = if style.color {
        (CYAN, DIM, RESET)
    } else {
        ("", "", "")
    };

    if detail.is_empty() {
        format!("{cyan}{frame}{reset} {status}")
    } else {
        format!("{cyan}{frame}{reset} {status}  {dim}{detail}{reset}")
    }
}

fn truncate_end(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }

    let mut kept: String = text.chars().take(max.saturating_sub(1)).collect();
    if max > 0 {
        kept.push('…');
    }
    kept
}

fn truncate_start(text: &str, max: usize) -> String {
    let len = text.chars().count();
    if len <= max {
        return text.to_string();
    }

    if max == 0 {
        return String::new();
    }

    let tail: String = text.chars().skip(len - (max - 1)).collect();
    format!("…{tail}")
}

/// Formats a count with thousands separators, e.g. `12,345`.
pub fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }

    grouped
}

#[cfg(test)]
mod tests {
    use super::{Line, Style, group_digits, render, truncate_end, truncate_start};

    #[test]
    fn groups_digits() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(12_345_678), "12,345,678");
    }

    #[test]
    fn truncates_keeping_the_relevant_end() {
        assert_eq!(truncate_end("Scanning", 20), "Scanning");
        assert_eq!(truncate_end("Scanning entries", 9), "Scanning…");
        assert_eq!(truncate_start("/home/user/apps/kika", 8), "…ps/kika");
        assert_eq!(truncate_start("/home", 0), "");
    }

    #[test]
    fn rendered_line_fits_the_terminal() {
        let line = Line {
            status: "Scanning 12,345 entries · 3 matches".to_string(),
            detail: "/home/user/apps/some/really/deep/project/path/node_modules".to_string(),
        };
        let style = Style {
            color: false,
            width: 60,
        };

        let rendered = render("⠋", &line, style);

        assert!(rendered.chars().count() < 60);
        assert!(rendered.ends_with("node_modules"));
    }
}
