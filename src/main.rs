mod cleanup;
mod cli;
mod groups;
mod progress;
mod tools;
mod tui;

use std::fs;
use std::io::{self, IsTerminal, Write};

use cleanup::{ScanReport, apply_cleanup, render_report, scan};
use cli::{Command, Config, parse_or_exit};
use groups::group_candidates;
use progress::{Spinner, group_digits};
use tui::Choice;

fn main() {
    let config = parse_or_exit();

    if let Err(message) = run(config) {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run(config: Config) -> Result<(), String> {
    let spinner = Spinner::start("Scanning");
    let report = scan(&config, &spinner)?;
    spinner.finish(&format!(
        "Scanned {} entries, found {} matches",
        group_digits(report.scanned_entries),
        group_digits(report.candidates.len() as u64)
    ));

    if is_interactive(&config) && !report.candidates.is_empty() {
        return run_interactive(&config, report);
    }

    let rendered = render_report(&config, &report);
    write_or_print_report(&config, &rendered)?;

    if config.should_apply() {
        if !confirm_apply(&config, &report)? {
            println!("Aborted. Nothing was deleted.");
            return Ok(());
        }

        clean(report);
    }

    Ok(())
}

/// The checklist only makes sense for a report shown to a person at a terminal.
fn is_interactive(config: &Config) -> bool {
    config.command == Command::Report
        && !config.plain
        && config.output.is_none()
        && io::stdin().is_terminal()
        && io::stdout().is_terminal()
}

fn run_interactive(config: &Config, report: ScanReport) -> Result<(), String> {
    let root = config
        .root
        .canonicalize()
        .map_err(|error| format!("could not access {}: {error}", config.root.display()))?;
    let ScanReport {
        candidates,
        warnings,
        ..
    } = report;

    let choice = tui::select(group_candidates(&root, candidates), warnings.len())
        .map_err(|error| format!("interactive screen failed: {error}"))?;

    if !warnings.is_empty() {
        eprintln!("Warnings:");
        for warning in &warnings {
            eprintln!("  WARN {warning}");
        }
    }

    match choice {
        Choice::Quit => println!("Nothing was deleted."),
        Choice::Clean(candidates) => clean(ScanReport {
            candidates,
            ..ScanReport::default()
        }),
    }

    Ok(())
}

fn clean(report: ScanReport) {
    let spinner = Spinner::start("Cleaning");
    let outcome = apply_cleanup(report, &spinner);
    spinner.finish("Cleanup finished");
    outcome.print();
}

fn write_or_print_report(config: &Config, rendered: &str) -> Result<(), String> {
    if let Some(path) = &config.output {
        fs::write(path, rendered)
            .map_err(|error| format!("could not write report to {}: {error}", path.display()))?;
        println!("Report written to {}", path.display());
        return Ok(());
    }

    print!("{rendered}");
    io::stdout()
        .flush()
        .map_err(|error| format!("could not flush report output: {error}"))?;
    Ok(())
}

fn confirm_apply(config: &Config, report: &cleanup::ScanReport) -> Result<bool, String> {
    if !config.should_apply() || report.candidates.is_empty() || config.yes {
        return Ok(true);
    }

    let mut stderr = io::stderr();

    if !io::stdin().is_terminal() {
        return Err(
            "clean requires terminal confirmation; re-run with --yes to skip the prompt"
                .to_string(),
        );
    }

    writeln!(
        stderr,
        "\nAbout to delete {} paths and reclaim an estimated {}.",
        report.candidates.len(),
        human_bytes(report.total_bytes())
    )
    .map_err(|error| format!("could not write confirmation prompt: {error}"))?;
    write!(stderr, "Proceed? [y/N]: ")
        .map_err(|error| format!("could not write confirmation prompt: {error}"))?;
    stderr
        .flush()
        .map_err(|error| format!("could not flush confirmation prompt: {error}"))?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("could not read confirmation input: {error}"))?;

    Ok(is_confirmation(&answer))
}

fn is_confirmation(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

    let mut value = bytes as f64;
    let mut unit_index = 0;

    while value >= 1024.0 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{bytes} {}", UNITS[unit_index])
    } else {
        format!("{value:.1} {}", UNITS[unit_index])
    }
}

#[cfg(test)]
mod tests {
    use super::is_confirmation;

    #[test]
    fn accepts_yes_answers() {
        assert!(is_confirmation("y"));
        assert!(is_confirmation("Y"));
        assert!(is_confirmation(" yes \n"));
    }

    #[test]
    fn rejects_non_yes_answers() {
        assert!(!is_confirmation(""));
        assert!(!is_confirmation("n"));
        assert!(!is_confirmation("no"));
    }
}
