use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Clean,
    Report,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub command: Command,
    pub root: PathBuf,
    pub apply: bool,
    pub yes: bool,
    pub include_old_files: bool,
    pub older_than_days: Option<u64>,
    pub output: Option<PathBuf>,
}

impl Config {
    pub fn parse_from<I, T>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        let mut args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        let mut command = Command::Clean;

        if let Some(first) = args.first().and_then(|value| value.to_str()) {
            match first {
                "clean" => {
                    command = Command::Clean;
                    args.remove(0);
                }
                "report" => {
                    command = Command::Report;
                    args.remove(0);
                }
                "-h" | "--help" => return Err(help_message(None)),
                _ => {}
            }
        }

        let mut apply = false;
        let mut yes = false;
        let mut include_old_files = false;
        let mut older_than_days = None;
        let mut output = None;
        let mut root = None;

        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            let arg = arg.to_string_lossy();

            match arg.as_ref() {
                "--apply" => apply = true,
                "--yes" => yes = true,
                "--include-old-files" => include_old_files = true,
                "--older-than-days" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "missing value for --older-than-days".to_string())?;
                    let value = value.to_string_lossy();
                    let parsed = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid number for --older-than-days: {value}"))?;
                    older_than_days = Some(parsed);
                }
                "--output" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "missing value for --output".to_string())?;
                    output = Some(PathBuf::from(value));
                }
                "-h" | "--help" => return Err(help_message(None)),
                _ if arg.starts_with('-') => return Err(format!("unknown flag: {arg}")),
                _ => {
                    if root.is_some() {
                        return Err("only one root path can be provided".to_string());
                    }

                    root = Some(PathBuf::from(arg.as_ref()));
                }
            }
        }

        if include_old_files && older_than_days.is_none() {
            return Err(
                "--include-old-files requires --older-than-days to define the cutoff".to_string(),
            );
        }

        if !include_old_files && older_than_days.is_some() {
            return Err(
                "--older-than-days requires --include-old-files to opt into old file cleanup"
                    .to_string(),
            );
        }

        if matches!(command, Command::Report) && apply {
            return Err("the report command does not support --apply".to_string());
        }

        if matches!(command, Command::Report) && yes {
            return Err("the report command does not support --yes".to_string());
        }

        if yes && !apply {
            return Err("--yes requires --apply".to_string());
        }

        Ok(Self {
            command,
            root: root.unwrap_or_else(|| PathBuf::from(".")),
            apply,
            yes,
            include_old_files,
            older_than_days,
            output,
        })
    }

    pub fn cleanup_old_files(&self) -> bool {
        self.include_old_files && self.older_than_days.is_some()
    }

    pub fn should_apply(&self) -> bool {
        matches!(self.command, Command::Clean) && self.apply
    }

    pub fn mode_label(&self) -> &'static str {
        match self.command {
            Command::Report => "report",
            Command::Clean if self.apply => "apply",
            Command::Clean => "dry-run",
        }
    }
}

pub fn parse_or_exit() -> Config {
    let mut args = env::args_os();
    let bin = args
        .next()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kika".to_string());

    match Config::parse_from(args) {
        Ok(config) => config,
        Err(message) if looks_like_help(&message) => {
            println!("{}", help_message(Some(&bin)));
            std::process::exit(0);
        }
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            eprintln!("{}", help_message(Some(&bin)));
            std::process::exit(1);
        }
    }
}

fn looks_like_help(message: &str) -> bool {
    message.starts_with("Usage:")
}

fn help_message(bin: Option<&str>) -> String {
    let bin = bin.unwrap_or("kika");

    format!(
        "Usage:
  {bin} [path] [--apply] [--yes] [--output <file>] [--include-old-files --older-than-days <days>]
  {bin} clean [path] [--apply] [--yes] [--output <file>] [--include-old-files --older-than-days <days>]
  {bin} report [path] [--output <file>] [--include-old-files --older-than-days <days>]

Commands:
  clean   Scan and optionally remove matched paths
  report  Generate a report without removing anything

Scans a directory tree looking for:
  - node_modules directories
  - dist directories
  - .next directories
  - temporary files (.tmp, .temp, .bak, .old, .orig, .rej, .swp, .swo)
  - Zone.Identifier files (Windows/WSL alternate data streams)

By default the command runs in dry-run mode and only reports what would be removed.
Use clean --apply to delete the matched files and directories.
Without --yes, the clean command asks for confirmation before removing anything.

Options:
  --apply                 Actually remove matched paths
  --yes                   Skip the confirmation prompt for clean --apply
  --output <file>         Save the report to a file
  --include-old-files     Also match regular files older than the cutoff
  --older-than-days <n>   Age threshold for old file cleanup
  -h, --help              Show this help message

Examples:
  {bin}
  {bin} clean /projects/frontend --apply
  {bin} clean /projects/frontend --apply --yes
  {bin} report /projects/frontend --output cleanup-report.txt
  {bin} report /projects/archive --include-old-files --older-than-days 90"
    )
}

#[cfg(test)]
mod tests {
    use super::{Command, Config};

    #[test]
    fn parses_default_root() {
        let config = Config::parse_from(Vec::<String>::new()).unwrap();
        assert_eq!(config.command, Command::Clean);
        assert_eq!(config.root, std::path::PathBuf::from("."));
        assert!(!config.apply);
        assert!(!config.yes);
    }

    #[test]
    fn parses_report_command_with_output() {
        let config =
            Config::parse_from(vec!["report", "/tmp/work", "--output", "report.txt"]).unwrap();

        assert_eq!(config.command, Command::Report);
        assert_eq!(config.root, std::path::PathBuf::from("/tmp/work"));
        assert_eq!(config.output, Some(std::path::PathBuf::from("report.txt")));
    }

    #[test]
    fn requires_old_file_cutoff_pairing() {
        let error = Config::parse_from(vec!["--include-old-files"]).unwrap_err();
        assert!(error.contains("--older-than-days"));

        let error = Config::parse_from(vec!["--older-than-days", "30"]).unwrap_err();
        assert!(error.contains("--include-old-files"));
    }

    #[test]
    fn rejects_apply_on_report_command() {
        let error = Config::parse_from(vec!["report", "--apply"]).unwrap_err();
        assert!(error.contains("does not support --apply"));
    }

    #[test]
    fn parses_yes_for_apply() {
        let config = Config::parse_from(vec!["clean", "--apply", "--yes"]).unwrap();
        assert!(config.apply);
        assert!(config.yes);
    }

    #[test]
    fn rejects_yes_without_apply() {
        let error = Config::parse_from(vec!["clean", "--yes"]).unwrap_err();
        assert!(error.contains("--yes requires --apply"));
    }
}
