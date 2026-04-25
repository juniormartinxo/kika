use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::cli::{Command, Config};

const REMOVABLE_DIRS: &[(&str, CleanupKind)] = &[
    ("node_modules", CleanupKind::NodeModules),
    ("dist", CleanupKind::Dist),
    (".next", CleanupKind::NextJs),
];

const TEMP_FILE_NAMES: &[&str] = &[".DS_Store", "Thumbs.db"];
const TEMP_FILE_EXTENSIONS: &[&str] = &["tmp", "temp", "bak", "old", "orig", "rej", "swp", "swo"];
const TEMP_FILE_SUFFIXES: &[&str] = &["Zone.Identifier"];
const PROTECTED_DIRS: &[&str] = &[".git", "target"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CleanupKind {
    NodeModules,
    Dist,
    NextJs,
    TempFile,
    OldFile,
}

impl CleanupKind {
    fn label(self) -> &'static str {
        match self {
            Self::NodeModules => "node_modules",
            Self::Dist => "dist",
            Self::NextJs => ".next",
            Self::TempFile => "temp-file",
            Self::OldFile => "old-file",
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

pub fn scan(config: &Config) -> Result<ScanReport, String> {
    let root = config
        .root
        .canonicalize()
        .map_err(|error| format!("could not access {}: {error}", config.root.display()))?;

    let metadata = fs::metadata(&root)
        .map_err(|error| format!("could not read metadata for {}: {error}", root.display()))?;

    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }

    let cutoff = cutoff_for_old_files(config);
    let mut scanner = Scanner::new(cutoff);
    scanner.scan_dir(&root);
    scanner
        .report
        .candidates
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(scanner.report)
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
        output.push('\n');

        match config.command {
            Command::Report => {
                output.push_str(
                    "Nothing was deleted. Use `clean --apply` to remove the matched paths.\n",
                );
            }
            Command::Clean => {
                output.push_str(
                    "Nothing was deleted. Re-run with --apply to remove the matched paths.\n",
                );
            }
        }
    }

    output
}

pub fn apply_cleanup(report: ScanReport) -> ApplyOutcome {
    let mut deleted = Vec::new();
    let mut failures = Vec::new();

    for candidate in report.candidates {
        match remove_candidate(&candidate) {
            Ok(()) => deleted.push(candidate),
            Err(error) => failures.push(format!("{}: {}", candidate.path.display(), error)),
        }
    }

    ApplyOutcome { deleted, failures }
}

struct Scanner {
    cutoff: Option<SystemTime>,
    report: ScanReport,
}

impl Scanner {
    fn new(cutoff: Option<SystemTime>) -> Self {
        Self {
            cutoff,
            report: ScanReport::default(),
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

        for entry in entries {
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

                if let Some(kind) = removable_dir_kind(name.as_ref()) {
                    let size_bytes = measure_dir(&path, &mut self.report.warnings);
                    self.report.candidates.push(CleanupCandidate {
                        path,
                        kind,
                        size_bytes,
                    });
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

fn remove_candidate(candidate: &CleanupCandidate) -> io::Result<()> {
    match candidate.kind {
        CleanupKind::NodeModules | CleanupKind::Dist | CleanupKind::NextJs => {
            fs::remove_dir_all(&candidate.path)
        }
        CleanupKind::TempFile | CleanupKind::OldFile => fs::remove_file(&candidate.path),
    }
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

fn format_bytes(bytes: u64) -> String {
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
        CleanupCandidate, CleanupKind, ScanReport, matches_temp_file, removable_dir_kind,
        render_report,
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
            apply: false,
            yes: false,
            include_old_files: false,
            older_than_days: None,
            output: None,
        };
        let report = ScanReport {
            candidates: vec![CleanupCandidate {
                path: Path::new("/tmp/work/node_modules").to_path_buf(),
                kind: CleanupKind::NodeModules,
                size_bytes: 1024,
            }],
            warnings: Vec::new(),
        };

        let rendered = render_report(&config, &report);

        assert!(rendered.contains("Mode: report"));
        assert!(rendered.contains("Root: /tmp/work"));
        assert!(rendered.contains("Nothing was deleted. Use `clean --apply`"));
    }
}
