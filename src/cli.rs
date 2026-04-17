use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub apply: bool,
    pub include_old_files: bool,
    pub older_than_days: Option<u64>,
}

impl Config {
    pub fn parse_from<I, T>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        let mut apply = false;
        let mut include_old_files = false;
        let mut older_than_days = None;
        let mut root = None;

        let mut iter = args.into_iter().map(Into::into);

        while let Some(arg) = iter.next() {
            let arg = arg.to_string_lossy();

            match arg.as_ref() {
                "--apply" => apply = true,
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

        Ok(Self {
            root: root.unwrap_or_else(|| PathBuf::from(".")),
            apply,
            include_old_files,
            older_than_days,
        })
    }

    pub fn cleanup_old_files(&self) -> bool {
        self.include_old_files && self.older_than_days.is_some()
    }
}

pub fn parse_or_exit() -> Config {
    let mut args = env::args_os();
    let bin = args
        .next()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kika-cleaner".to_string());

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
    let bin = bin.unwrap_or("kika-cleaner");

    format!(
        "Usage: {bin} [path] [--apply] [--include-old-files --older-than-days <days>]

Scans a directory tree looking for:
  - node_modules directories
  - dist directories
  - .next directories
  - temporary files (.tmp, .temp, .bak, .old, .orig, .rej, .swp, .swo)

By default the command runs in dry-run mode and only reports what would be removed.
Use --apply to delete the matched files and directories.

Options:
  --apply                 Actually remove matched paths
  --include-old-files     Also match regular files older than the cutoff
  --older-than-days <n>   Age threshold for old file cleanup
  -h, --help              Show this help message

Examples:
  {bin}
  {bin} /projects/frontend
  {bin} /projects/frontend --apply
  {bin} /projects/archive --apply --include-old-files --older-than-days 90"
    )
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn parses_default_root() {
        let config = Config::parse_from(Vec::<String>::new()).unwrap();
        assert_eq!(config.root, std::path::PathBuf::from("."));
        assert!(!config.apply);
    }

    #[test]
    fn requires_old_file_cutoff_pairing() {
        let error = Config::parse_from(vec!["--include-old-files"]).unwrap_err();
        assert!(error.contains("--older-than-days"));

        let error = Config::parse_from(vec!["--older-than-days", "30"]).unwrap_err();
        assert!(error.contains("--include-old-files"));
    }
}
