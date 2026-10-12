use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::cli::{Config, GlobalCache};
use crate::progress::{Spinner, group_digits};
use crate::tools::Tool;

const REMOVABLE_DIRS: &[(&str, CleanupKind)] = &[
    ("node_modules", CleanupKind::NodeModules),
    ("dist", CleanupKind::Dist),
    (".next", CleanupKind::NextJs),
];

const TEMP_FILE_NAMES: &[&str] = &[".DS_Store", "Thumbs.db"];
const TEMP_FILE_EXTENSIONS: &[&str] = &["tmp", "temp", "bak", "old", "orig", "rej", "swp", "swo"];
const TEMP_FILE_SUFFIXES: &[&str] = &["Zone.Identifier"];
const PROTECTED_DIRS: &[&str] = &[".git"];

/// A cache owned by a package manager: located with one command, cleaned with another.
struct ToolCache {
    kind: CleanupKind,
    program: &'static str,
    locate_args: &'static [&'static str],
    clean_args: &'static [&'static str],
    /// Part of the located directory that the clean command actually empties.
    subdir: Option<&'static str>,
}

const TOOL_CACHES: &[ToolCache] = &[
    ToolCache {
        kind: CleanupKind::PnpmStore,
        program: "pnpm",
        locate_args: &["store", "path"],
        clean_args: &["store", "prune"],
        subdir: None,
    },
    ToolCache {
        kind: CleanupKind::UvCache,
        program: "uv",
        locate_args: &["cache", "dir"],
        clean_args: &["cache", "clean"],
        subdir: None,
    },
    ToolCache {
        kind: CleanupKind::YarnCache,
        program: "yarn",
        locate_args: &["cache", "dir"],
        clean_args: &["cache", "clean"],
        subdir: None,
    },
    ToolCache {
        kind: CleanupKind::NpmCache,
        program: "npm",
        locate_args: &["config", "get", "cache"],
        clean_args: &["cache", "clean", "--force"],
        subdir: Some("_cacache"),
    },
    ToolCache {
        kind: CleanupKind::GoBuildCache,
        program: "go",
        locate_args: &["env", "GOCACHE"],
        clean_args: &["clean", "-cache"],
        subdir: None,
    },
    ToolCache {
        kind: CleanupKind::GoModCache,
        program: "go",
        locate_args: &["env", "GOMODCACHE"],
        clean_args: &["clean", "-modcache"],
        subdir: None,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CleanupKind {
    NodeModules,
    Dist,
    NextJs,
    CargoTarget,
    DockerBuildCache,
    PnpmStore,
    UvCache,
    YarnCache,
    NpmCache,
    GoBuildCache,
    GoModCache,
    TempFile,
    OldFile,
}

impl CleanupKind {
    fn label(self) -> &'static str {
        match self {
            Self::NodeModules => "node_modules",
            Self::Dist => "dist",
            Self::NextJs => ".next",
            Self::CargoTarget => "cargo-target",
            Self::DockerBuildCache => "docker-cache",
            Self::PnpmStore => "pnpm-store",
            Self::UvCache => "uv-cache",
            Self::YarnCache => "yarn-cache",
            Self::NpmCache => "npm-cache",
            Self::GoBuildCache => "go-cache",
            Self::GoModCache => "go-modcache",
            Self::TempFile => "temp-file",
            Self::OldFile => "old-file",
        }
    }

    /// Human name of a global cache; `None` for kinds found by the directory scan.
    pub fn global_cache_name(self) -> Option<&'static str> {
        match self {
            Self::DockerBuildCache => Some("Docker build cache"),
            Self::PnpmStore => Some("pnpm store"),
            Self::UvCache => Some("uv cache"),
            Self::YarnCache => Some("yarn cache"),
            Self::NpmCache => Some("npm cache"),
            Self::GoBuildCache => Some("Go build cache"),
            Self::GoModCache => Some("Go module cache"),
            Self::NodeModules
            | Self::Dist
            | Self::NextJs
            | Self::CargoTarget
            | Self::TempFile
            | Self::OldFile => None,
        }
    }
}

impl fmt::Display for CleanupKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone)]
pub struct CleanupCandidate {
    pub path: PathBuf,
    pub kind: CleanupKind,
    pub size_bytes: u64,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub candidates: Vec<CleanupCandidate>,
    pub warnings: Vec<String>,
    pub scanned_entries: u64,
}

impl ScanReport {
    pub fn total_bytes(&self) -> u64 {
        self.candidates
            .iter()
            .map(|candidate| candidate.size_bytes)
            .sum()
    }

    fn counts_by_kind(&self) -> BTreeMap<CleanupKind, usize> {
        let mut counts = BTreeMap::new();

        for candidate in &self.candidates {
            *counts.entry(candidate.kind).or_insert(0) += 1;
        }

        counts
    }
}

pub struct ApplyOutcome {
    deleted: Vec<CleanupCandidate>,
    failures: Vec<String>,
}

impl ApplyOutcome {
    pub fn print(&self) {
        if self.deleted.is_empty() && self.failures.is_empty() {
            println!("No paths were removed.");
            return;
        }

        if !self.deleted.is_empty() {
            println!();
            println!("Removed:");

            for candidate in &self.deleted {
                println!(
                    "  DEL  [{:<12}] {} ({})",
                    candidate.kind,
                    candidate.path.display(),
                    format_bytes(candidate.size_bytes)
                );
            }
        }

        if !self.failures.is_empty() {
            println!();
            println!("Failures:");

            for failure in &self.failures {
                println!("  ERR  {failure}");
            }
        }

        let total_bytes: u64 = self
            .deleted
            .iter()
            .map(|candidate| candidate.size_bytes)
            .sum();

        println!();
        println!(
            "Apply summary: {} removed, {} failed, {} reclaimed.",
            self.deleted.len(),
            self.failures.len(),
            format_bytes(total_bytes)
        );
    }
}

/// Absolute, symlink-free form of the scanned root. On Windows `canonicalize`
/// returns verbatim paths (`\\?\C:\...`); plain drive paths go back to the
/// familiar `C:\...` form so reports stay readable.
pub fn canonical_root(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize().map(strip_verbatim_prefix)
}

fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};

        let is_disk = matches!(
            path.components().next(),
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::VerbatimDisk(_))
        );
        if is_disk && let Some(plain) = path.to_str().and_then(|value| value.strip_prefix(r"\\?\"))
        {
            return PathBuf::from(plain);
        }
    }

    path
}

pub fn scan(config: &Config, spinner: &Spinner) -> Result<ScanReport, String> {
    let root = canonical_root(&config.root)
        .map_err(|error| format!("could not access {}: {error}", config.root.display()))?;

    let metadata = fs::metadata(&root)
        .map_err(|error| format!("could not read metadata for {}: {error}", root.display()))?;

    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }

    let cutoff = cutoff_for_old_files(config);
    let mut scanner = Scanner::new(cutoff, spinner);
    scanner.scan_dir(&root);
    scanner
        .report
        .candidates
        .sort_by(|left, right| left.path.cmp(&right.path));

    let mut report = scanner.report;
    scan_global_caches(config, &mut report, spinner);
    Ok(report)
}

fn scan_global_caches(config: &Config, report: &mut ScanReport, spinner: &Spinner) {
    for cache in &config.global_caches {
        spinner.update(|| {
            (
                format!("Checking {}", global_cache_name(*cache)),
                String::new(),
            )
        });

        let result = match cache {
            GlobalCache::DockerBuilder => docker_build_cache_candidate(),
            GlobalCache::PnpmStore => tool_cache_candidate(CleanupKind::PnpmStore, report),
            GlobalCache::Uv => tool_cache_candidate(CleanupKind::UvCache, report),
            GlobalCache::Yarn => tool_cache_candidate(CleanupKind::YarnCache, report),
            GlobalCache::Npm => tool_cache_candidate(CleanupKind::NpmCache, report),
            GlobalCache::GoBuild => tool_cache_candidate(CleanupKind::GoBuildCache, report),
            GlobalCache::GoModules => tool_cache_candidate(CleanupKind::GoModCache, report),
        };

        match result {
            Ok(candidate) => report.candidates.push(candidate),
            Err(error) => report
                .warnings
                .push(format!("skipping {}: {error}", global_cache_name(*cache))),
        }
    }
}

fn global_cache_name(cache: GlobalCache) -> &'static str {
    match cache {
        GlobalCache::DockerBuilder => "Docker build cache",
        GlobalCache::PnpmStore => "pnpm store",
        GlobalCache::Uv => "uv cache",
        GlobalCache::Yarn => "yarn cache",
        GlobalCache::Npm => "npm cache",
        GlobalCache::GoBuild => "Go build cache",
        GlobalCache::GoModules => "Go module cache",
    }
}

pub fn render_report(config: &Config, report: &ScanReport) -> String {
    let mut output = String::new();

    output.push_str(&format!("Mode: {}\n", config.mode_label()));
    output.push_str(&format!("Root: {}\n", config.root.display()));
    output.push_str(&format!("Matches: {}\n", report.candidates.len()));
    output.push_str(&format!(
        "Estimated reclaimable space: {}\n",
        format_bytes(report.total_bytes())
    ));

    if let Some(days) = config.older_than_days {
        output.push_str(&format!("Old file cutoff: {days} days\n"));
    }

    let counts = report.counts_by_kind();
    if !counts.is_empty() {
        output.push_str("\nBy kind:\n");

        for (kind, count) in counts {
            output.push_str(&format!("  - {:<12} {count}\n", kind));
        }
    }

    if !report.candidates.is_empty() {
        output.push_str("\nCandidates:\n");

        for candidate in &report.candidates {
            output.push_str(&format!(
                "  {} [{:<12}] {} ({})\n",
                candidate_prefix(config),
                candidate.kind,
                candidate.path.display(),
                format_bytes(candidate.size_bytes)
            ));
        }
    }

    if !report.warnings.is_empty() {
        output.push_str("\nWarnings:\n");

        for warning in &report.warnings {
            output.push_str(&format!("  WARN {warning}\n"));
        }
    }

    if !config.should_apply() {
        output.push_str("\nNothing was deleted. Run `kika clean` to remove the matched paths.\n");
    }

    output
}

pub fn apply_cleanup(report: ScanReport, spinner: &Spinner) -> ApplyOutcome {
    let mut deleted = Vec::new();
    let mut failures = Vec::new();
    let total = report.candidates.len();

    for (index, candidate) in report.candidates.into_iter().enumerate() {
        spinner.update(|| {
            (
                format!("Cleaning {}/{total} [{}]", index + 1, candidate.kind),
                candidate.path.display().to_string(),
            )
        });

        match remove_candidate(&candidate) {
            Ok(reclaimed) => deleted.push(CleanupCandidate {
                size_bytes: reclaimed,
                ..candidate
            }),
            Err(error) => failures.push(format!("{}: {}", candidate.path.display(), error)),
        }
    }

    ApplyOutcome { deleted, failures }
}

struct Scanner<'a> {
    cutoff: Option<SystemTime>,
    report: ScanReport,
    spinner: &'a Spinner,
}

impl<'a> Scanner<'a> {
    fn new(cutoff: Option<SystemTime>, spinner: &'a Spinner) -> Self {
        Self {
            cutoff,
            report: ScanReport::default(),
            spinner,
        }
    }

    fn scan_dir(&mut self, dir: &Path) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.warn(format!("could not read {}: {error}", dir.display()));
                return;
            }
        };

        self.spinner.update(|| {
            (
                format!(
                    "Scanning {} entries · {} matches",
                    group_digits(self.report.scanned_entries),
                    group_digits(self.report.candidates.len() as u64)
                ),
                dir.display().to_string(),
            )
        });

        for entry in entries {
            self.report.scanned_entries += 1;

            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.warn(format!(
                        "could not read an entry in {}: {error}",
                        dir.display()
                    ));
                    continue;
                }
            };

            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    self.warn(format!("could not inspect {}: {error}", path.display()));
                    continue;
                }
            };

            if file_type.is_symlink() {
                continue;
            }

            let name = entry.file_name();
            let name = name.to_string_lossy();

            if file_type.is_dir() {
                if PROTECTED_DIRS.contains(&name.as_ref()) {
                    continue;
                }

                if name.as_ref() == "target" {
                    if dir.join("Cargo.toml").is_file() {
                        self.push_dir_candidate(path, CleanupKind::CargoTarget);
                    }
                    continue;
                }

                if let Some(kind) = removable_dir_kind(name.as_ref()) {
                    self.push_dir_candidate(path, kind);
                    continue;
                }

                self.scan_dir(&path);
                continue;
            }

            if !file_type.is_file() {
                continue;
            }

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    self.warn(format!(
                        "could not read metadata for {}: {error}",
                        path.display()
                    ));
                    continue;
                }
            };

            if matches_temp_file(path.as_path(), name.as_ref()) {
                self.report.candidates.push(CleanupCandidate {
                    path,
                    kind: CleanupKind::TempFile,
                    size_bytes: metadata.len(),
                });
                continue;
            }

            if self.matches_old_file(&metadata) {
                self.report.candidates.push(CleanupCandidate {
                    path,
                    kind: CleanupKind::OldFile,
                    size_bytes: metadata.len(),
                });
            }
        }
    }

    fn push_dir_candidate(&mut self, path: PathBuf, kind: CleanupKind) {
        self.spinner
            .update(|| (format!("Measuring {kind}"), path.display().to_string()));

        let size_bytes = measure_dir(&path, &mut self.report.warnings);
        self.report.candidates.push(CleanupCandidate {
            path,
            kind,
            size_bytes,
        });
    }

    fn matches_old_file(&mut self, metadata: &fs::Metadata) -> bool {
        let Some(cutoff) = self.cutoff else {
            return false;
        };

        let modified = match metadata.modified() {
            Ok(value) => value,
            Err(error) => {
                self.warn(format!("could not read modified timestamp: {error}"));
                return false;
            }
        };

        modified <= cutoff
    }

    fn warn(&mut self, message: String) {
        self.report.warnings.push(message);
    }
}

fn cutoff_for_old_files(config: &Config) -> Option<SystemTime> {
    if !config.cleanup_old_files() {
        return None;
    }

    let days = config.older_than_days?;
    let seconds = days.saturating_mul(24 * 60 * 60);
    Some(SystemTime::now() - Duration::from_secs(seconds))
}

/// Removes the candidate and returns how many bytes were actually reclaimed.
fn remove_candidate(candidate: &CleanupCandidate) -> io::Result<u64> {
    match candidate.kind {
        CleanupKind::NodeModules | CleanupKind::Dist | CleanupKind::NextJs => {
            fs::remove_dir_all(&candidate.path)?;
            Ok(candidate.size_bytes)
        }
        CleanupKind::CargoTarget => {
            cargo_clean(&candidate.path)?;
            Ok(candidate.size_bytes)
        }
        CleanupKind::DockerBuildCache => docker_builder_prune(candidate.size_bytes),
        CleanupKind::PnpmStore
        | CleanupKind::UvCache
        | CleanupKind::YarnCache
        | CleanupKind::NpmCache
        | CleanupKind::GoBuildCache
        | CleanupKind::GoModCache => clean_tool_cache(candidate),
        CleanupKind::TempFile | CleanupKind::OldFile => {
            fs::remove_file(&candidate.path)?;
            Ok(candidate.size_bytes)
        }
    }
}

fn cargo_clean(target_dir: &Path) -> io::Result<()> {
    let project_dir = target_dir.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", target_dir.display()),
        )
    })?;

    let cargo = Tool::find("cargo").map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "cargo not found in PATH; skipping (install cargo or remove target/ manually)",
        )
    })?;

    cargo.run(&["clean", "--offline"], Some(project_dir))?;
    Ok(())
}

fn docker_build_cache_candidate() -> io::Result<CleanupCandidate> {
    let docker = Tool::find("docker")?;
    let stdout = docker.run(
        &["system", "df", "--format", "{{.Type}}\t{{.Reclaimable}}"],
        None,
    )?;

    let reclaimable = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Build Cache\t"))
        .ok_or_else(|| io::Error::other("build cache not listed by `docker system df`"))?;

    let size_bytes = parse_docker_size(reclaimable)
        .ok_or_else(|| io::Error::other(format!("could not parse Docker size: {reclaimable}")))?;

    Ok(CleanupCandidate {
        path: PathBuf::from("<docker build cache>"),
        kind: CleanupKind::DockerBuildCache,
        size_bytes,
    })
}

fn docker_builder_prune(estimated_bytes: u64) -> io::Result<u64> {
    let stdout = Tool::find("docker")?.run(&["builder", "prune", "--force"], None)?;

    // buildx prints "Total:\t<size>", the legacy builder "Total reclaimed space: <size>".
    let reclaimed = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with("Total"))
        .and_then(|line| line.split_whitespace().last())
        .and_then(parse_docker_size);

    Ok(reclaimed.unwrap_or(estimated_bytes))
}

fn tool_cache(kind: CleanupKind) -> &'static ToolCache {
    TOOL_CACHES
        .iter()
        .find(|cache| cache.kind == kind)
        .expect("every tool cache kind has an entry in TOOL_CACHES")
}

fn tool_cache_candidate(
    kind: CleanupKind,
    report: &mut ScanReport,
) -> io::Result<CleanupCandidate> {
    let cache = tool_cache(kind);
    let stdout = Tool::find(cache.program)?.run(cache.locate_args, None)?;

    // Tools may print warnings before the path, so only the last line counts.
    let dir = stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("`{}` returned no path", cache.program)))?;
    let dir = match cache.subdir {
        Some(subdir) => dir.join(subdir),
        None => dir,
    };

    if !dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("cache directory not found at {}", dir.display()),
        ));
    }

    // Upper bound: some cleaners (e.g. `pnpm store prune`) keep referenced entries.
    let size_bytes = measure_dir(&dir, &mut report.warnings);

    Ok(CleanupCandidate {
        path: dir,
        kind,
        size_bytes,
    })
}

fn clean_tool_cache(candidate: &CleanupCandidate) -> io::Result<u64> {
    let cache = tool_cache(candidate.kind);
    Tool::find(cache.program)?.run(cache.clean_args, None)?;

    let size_after = if candidate.path.is_dir() {
        measure_dir(&candidate.path, &mut Vec::new())
    } else {
        0
    };

    Ok(candidate.size_bytes.saturating_sub(size_after))
}

/// Parses sizes printed by Docker (decimal units, e.g. `1.534GB`, `234kB`, `0B`).
fn parse_docker_size(value: &str) -> Option<u64> {
    let value = value.split_whitespace().next()?;
    let split = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number: f64 = number.parse().ok()?;

    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "pb" => 1e15,
        _ => return None,
    };

    Some((number * multiplier).round() as u64)
}

fn removable_dir_kind(name: &str) -> Option<CleanupKind> {
    REMOVABLE_DIRS
        .iter()
        .find_map(|(dir_name, kind)| (*dir_name == name).then_some(*kind))
}

fn matches_temp_file(path: &Path, name: &str) -> bool {
    if TEMP_FILE_NAMES.contains(&name) {
        return true;
    }

    if TEMP_FILE_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return true;
    }

    let extension = match path.extension().and_then(|value| value.to_str()) {
        Some(extension) => extension,
        None => return false,
    };

    TEMP_FILE_EXTENSIONS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(extension))
}

fn measure_dir(dir: &Path, warnings: &mut Vec<String>) -> u64 {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "could not read {} for sizing: {error}",
                dir.display()
            ));
            return 0;
        }
    };

    let mut total = 0_u64;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!(
                    "could not read an entry in {} for sizing: {error}",
                    dir.display()
                ));
                continue;
            }
        };

        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                warnings.push(format!(
                    "could not inspect {} for sizing: {error}",
                    path.display()
                ));
                continue;
            }
        };

        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_file() {
            match entry.metadata() {
                Ok(metadata) => total = total.saturating_add(metadata.len()),
                Err(error) => warnings.push(format!(
                    "could not read metadata for {} while sizing: {error}",
                    path.display()
                )),
            }
            continue;
        }

        if file_type.is_dir() {
            total = total.saturating_add(measure_dir(&path, warnings));
        }
    }

    total
}

pub fn format_bytes(bytes: u64) -> String {
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

fn candidate_prefix(config: &Config) -> &'static str {
    if config.should_apply() {
        "DEL?"
    } else {
        "DRY "
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CleanupCandidate, CleanupKind, ScanReport, matches_temp_file, parse_docker_size,
        removable_dir_kind, render_report,
    };
    use crate::cli::{Command, Config};
    use std::path::Path;

    #[test]
    fn detects_default_cleanup_directories() {
        assert_eq!(
            removable_dir_kind("node_modules"),
            Some(CleanupKind::NodeModules)
        );
        assert_eq!(removable_dir_kind("dist"), Some(CleanupKind::Dist));
        assert_eq!(removable_dir_kind(".next"), Some(CleanupKind::NextJs));
        assert_eq!(removable_dir_kind("src"), None);
    }

    #[test]
    fn detects_temp_files_by_name_and_extension() {
        assert!(matches_temp_file(Path::new("/tmp/.DS_Store"), ".DS_Store"));
        assert!(matches_temp_file(Path::new("/tmp/file.tmp"), "file.tmp"));
        assert!(matches_temp_file(Path::new("/tmp/file.TEMP"), "file.TEMP"));
        assert!(matches_temp_file(
            Path::new("/tmp/image.png:Zone.Identifier"),
            "image.png:Zone.Identifier"
        ));
        assert!(matches_temp_file(
            Path::new("/tmp/Zone.Identifier"),
            "Zone.Identifier"
        ));
        assert!(!matches_temp_file(Path::new("/tmp/file.ts"), "file.ts"));
    }

    #[test]
    fn renders_report_mode() {
        let config = Config {
            command: Command::Report,
            root: Path::new("/tmp/work").to_path_buf(),
            yes: false,
            include_old_files: false,
            older_than_days: None,
            output: None,
            global_caches: Default::default(),
            plain: false,
        };
        let report = ScanReport {
            candidates: vec![CleanupCandidate {
                path: Path::new("/tmp/work/node_modules").to_path_buf(),
                kind: CleanupKind::NodeModules,
                size_bytes: 1024,
            }],
            ..Default::default()
        };

        let rendered = render_report(&config, &report);

        assert!(rendered.contains("Mode: report"));
        assert!(rendered.contains("Root: /tmp/work"));
        assert!(rendered.contains("Nothing was deleted. Run `kika clean`"));
    }

    #[cfg(windows)]
    #[test]
    fn strips_verbatim_prefix_from_disk_paths() {
        use super::strip_verbatim_prefix;
        use std::path::PathBuf;

        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\C:\Users\junior")),
            PathBuf::from(r"C:\Users\junior")
        );
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\?\UNC\server\share")
        );
    }

    #[test]
    fn parses_docker_sizes() {
        assert_eq!(parse_docker_size("0B"), Some(0));
        assert_eq!(parse_docker_size("234kB (0%)"), Some(234_000));
        assert_eq!(parse_docker_size("1.534GB"), Some(1_534_000_000));
        assert_eq!(parse_docker_size("680.6MB"), Some(680_600_000));
        assert_eq!(parse_docker_size("12XB"), None);
        assert_eq!(parse_docker_size(""), None);
    }
}
