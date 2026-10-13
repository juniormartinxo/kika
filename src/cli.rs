use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Clean,
    Report,
}

/// Caches managed by external tools, outside the scanned directory tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlobalCache {
    DockerBuilder,
    PnpmStore,
    Uv,
    Yarn,
    Npm,
    GoBuild,
    GoModules,
}

impl GlobalCache {
    pub const ALL: [Self; 7] = [
        Self::DockerBuilder,
        Self::PnpmStore,
        Self::Uv,
        Self::Yarn,
        Self::Npm,
        Self::GoBuild,
        Self::GoModules,
    ];

    fn from_flag(flag: &str) -> Option<Self> {
        match flag {
            "--docker-builder" => Some(Self::DockerBuilder),
            "--pnpm-store" => Some(Self::PnpmStore),
            "--uv-cache" => Some(Self::Uv),
            "--yarn-cache" => Some(Self::Yarn),
            "--npm-cache" => Some(Self::Npm),
            "--go-cache" => Some(Self::GoBuild),
            "--go-modcache" => Some(Self::GoModules),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub command: Command,
    pub root: PathBuf,
    pub yes: bool,
    pub include_old_files: bool,
    pub older_than_days: Option<u64>,
    pub output: Option<PathBuf>,
    pub global_caches: BTreeSet<GlobalCache>,
    pub plain: bool,
}

impl Config {
    pub fn parse_from<I, T>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        let mut args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        let mut command = Command::Report;

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
        let mut selected_caches = BTreeSet::new();
        let mut no_caches = false;
        let mut plain = false;
        let mut root = None;

        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            let arg = arg.to_string_lossy();

            match arg.as_ref() {
                "--apply" => apply = true,
                "--yes" => yes = true,
                "--include-old-files" => include_old_files = true,
                "--all-caches" => selected_caches.extend(GlobalCache::ALL),
                "--no-caches" => no_caches = true,
                "--plain" => plain = true,
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
                flag if flag.starts_with('-') => {
                    let cache = GlobalCache::from_flag(flag)
                        .ok_or_else(|| format!("unknown flag: {flag}"))?;
                    selected_caches.insert(cache);
                }
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

        if no_caches && !selected_caches.is_empty() {
            return Err("--no-caches cannot be combined with cache flags".to_string());
        }

        // `clean` always applies; `--apply` is still accepted there for old scripts.
        if matches!(command, Command::Report) && apply {
            return Err("reports never delete anything; use `clean` to remove paths".to_string());
        }

        if matches!(command, Command::Report) && yes {
            return Err("--yes only applies to the clean command".to_string());
        }

        // Without cache flags every global cache is included.
        let global_caches = if no_caches {
            BTreeSet::new()
        } else if selected_caches.is_empty() {
            GlobalCache::ALL.into_iter().collect()
        } else {
            selected_caches
        };

        Ok(Self {
            command,
            root: root.unwrap_or_else(|| PathBuf::from(".")),
            yes,
            include_old_files,
            older_than_days,
            output,
            global_caches,
            plain,
        })
    }

    pub fn cleanup_old_files(&self) -> bool {
        self.include_old_files && self.older_than_days.is_some()
    }

    pub fn should_apply(&self) -> bool {
        matches!(self.command, Command::Clean)
    }

    pub fn mode_label(&self) -> &'static str {
        match self.command {
            Command::Report => "report",
            Command::Clean => "clean",
        }
    }
}

pub fn parse_or_exit() -> Config {
    let mut args = env::args_os();
    // `argv[0]` may be a full path (`C:\...\kika.exe`); the help only needs the name.
    let bin = args
        .next()
        .as_deref()
        .and_then(|value| Path::new(value).file_stem())
        .map(|stem| stem.to_string_lossy().into_owned())
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
  {bin} [report] [path] [--plain] [--output <file>] [cache flags] [--include-old-files --older-than-days <days>]
  {bin} clean [path] [--yes] [--output <file>] [cache flags] [--include-old-files --older-than-days <days>]

Commands:
  report  Show what can be removed, without deleting anything (default).
          In a terminal it opens an interactive list to pick what to clean.
  clean   Remove everything the report lists, after confirmation

Scans a directory tree looking for:
  - node_modules directories (and renamed copies like node_modules_old),
    dist and .next directories, all next to a package.json, inside a git
    repository or beside a lockfile (package-lock.json, pnpm-lock.yaml...)
  - target directories of Rust projects (cleaned via `cargo clean --offline`)
  - temporary files (.tmp, .temp, .bak, .old, .orig, .rej, .swp, .swo),
    only inside git repositories
  - .DS_Store, Thumbs.db and Zone.Identifier files, anywhere

Never looks inside hidden directories (.vscode, .cache, .git...), AppData,
Library, Applications, Program Files, ProgramData, Windows or installed
Electron apps (folders with app.asar or node_modules.asar).

Global caches (outside the scanned path, all included by default):
  - Docker build cache, cleaned via `docker builder prune --force`
  - pnpm store, cleaned via `pnpm store prune`
  - uv cache, cleaned via `uv cache clean`
  - yarn cache, cleaned via `yarn cache clean`
  - npm cache, cleaned via `npm cache clean --force`
  - Go build cache, cleaned via `go clean -cache`
  - Go module cache, cleaned via `go clean -modcache`

Without --yes, the clean command asks for confirmation before removing anything.

Options:
  --yes                   Skip the confirmation prompt of clean
  --output <file>         Save the report to a file
  --plain                 Print the text report instead of the interactive list
  --include-old-files     Also match regular files older than the cutoff
  --older-than-days <n>   Age threshold for old file cleanup
  -h, --help              Show this help message

Cache flags (pick a subset; without them every cache is included):
  --docker-builder        Docker build cache
  --pnpm-store            Unreferenced packages in the pnpm store
  --uv-cache              uv cache
  --yarn-cache            yarn cache
  --npm-cache             npm cache
  --go-cache              Go build cache
  --go-modcache           Go module cache (modules are downloaded again)
  --all-caches            Every cache above (the default)
  --no-caches             Skip global caches, only scan the path

Examples:
  {bin}
  {bin} ~/projects
  {bin} clean
  {bin} clean ~/projects --yes
  {bin} clean ~/projects --no-caches
  {bin} clean --docker-builder --pnpm-store
  {bin} report ~/projects --output cleanup-report.txt
  {bin} report ~/archive --include-old-files --older-than-days 90"
    )
}

#[cfg(test)]
mod tests {
    use super::{Command, Config, GlobalCache};

    #[test]
    fn defaults_to_full_report() {
        let config = Config::parse_from(Vec::<String>::new()).unwrap();
        assert_eq!(config.command, Command::Report);
        assert_eq!(config.root, std::path::PathBuf::from("."));
        assert!(!config.should_apply());
        assert_eq!(config.global_caches.len(), GlobalCache::ALL.len());
    }

    #[test]
    fn path_without_command_is_a_report() {
        let config = Config::parse_from(vec!["/tmp/work"]).unwrap();
        assert_eq!(config.command, Command::Report);
        assert_eq!(config.root, std::path::PathBuf::from("/tmp/work"));
    }

    #[test]
    fn clean_applies_with_every_cache() {
        let config = Config::parse_from(vec!["clean"]).unwrap();
        assert!(config.should_apply());
        assert!(!config.yes);
        assert_eq!(config.global_caches.len(), GlobalCache::ALL.len());
    }

    #[test]
    fn clean_still_accepts_apply_flag() {
        let config = Config::parse_from(vec!["clean", "--apply", "--yes"]).unwrap();
        assert!(config.should_apply());
        assert!(config.yes);
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
    fn rejects_apply_and_yes_on_reports() {
        let error = Config::parse_from(vec!["--apply"]).unwrap_err();
        assert!(error.contains("use `clean`"));

        let error = Config::parse_from(vec!["report", "--yes"]).unwrap_err();
        assert!(error.contains("--yes only applies to the clean command"));
    }

    #[test]
    fn cache_flags_select_a_subset() {
        let config = Config::parse_from(vec![
            "clean",
            "--docker-builder",
            "--npm-cache",
            "--go-modcache",
            "--npm-cache",
        ])
        .unwrap();
        assert_eq!(
            config.global_caches.into_iter().collect::<Vec<_>>(),
            vec![
                GlobalCache::DockerBuilder,
                GlobalCache::Npm,
                GlobalCache::GoModules
            ]
        );
    }

    #[test]
    fn no_caches_skips_global_caches() {
        let config = Config::parse_from(vec!["clean", "--no-caches"]).unwrap();
        assert!(config.global_caches.is_empty());

        let error = Config::parse_from(vec!["--no-caches", "--uv-cache"]).unwrap_err();
        assert!(error.contains("cannot be combined"));
    }

    #[test]
    fn parses_plain_flag() {
        assert!(!Config::parse_from(Vec::<String>::new()).unwrap().plain);
        assert!(Config::parse_from(vec!["--plain"]).unwrap().plain);
    }

    #[test]
    fn rejects_unknown_flag() {
        let error = Config::parse_from(vec!["--bogus"]).unwrap_err();
        assert!(error.contains("unknown flag: --bogus"));
    }
}
