use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::cli::{Config, GlobalCache};
use crate::progress::{Spinner, group_digits};
use crate::tools::{Tool, home_dir};

/// Build outputs of JavaScript projects. They only count next to a
/// `package.json` of a project under development (see `is_js_project`), so
/// folders that merely share the name are left alone.
const PROJECT_DIRS: &[(&str, CleanupKind)] =
    &[("dist", CleanupKind::Dist), (".next", CleanupKind::NextJs)];

/// `node_modules` and its renamed copies (`node_modules_old`, `node_modules.bak`).
const NODE_MODULES_PREFIX: &str = "node_modules";

/// Never descended into: they hold installed programs, editor extensions and
/// system files, whose `node_modules` and `dist` are not project leftovers.
/// Hidden directories (`.vscode`, `.cache`, `.git`...) are skipped as well.
const SKIPPED_DIRS: &[&str] = &[
    "AppData",
    "Library",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "Windows",
    "$Recycle.Bin",
    "Applications",
];

/// Lockfiles only exist where someone ran the package manager. Installed
/// programs ship a `package.json` beside their `node_modules`, but no lockfile.
const JS_LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
];

/// Archives of a packaged Electron app (Dusky, Cursor, VS Code...). A directory
/// holding one is an installed program: nothing in it is a project leftover.
const ELECTRON_ARCHIVES: &[&str] = &["app.asar", "node_modules.asar"];

/// Junk anywhere: these files never hold anything worth keeping.
const JUNK_FILE_NAMES: &[&str] = &[".DS_Store", "Thumbs.db"];
const JUNK_FILE_SUFFIXES: &[&str] = &["Zone.Identifier"];

/// Leftovers of editors and merges. Outside a git repository a `.bak` is often
/// someone's only backup, so these only count inside one.
const TEMP_FILE_EXTENSIONS: &[&str] = &["tmp", "temp", "bak", "old", "orig", "rej", "swp", "swo"];

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
    PnpmOldStore,
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
            Self::PnpmOldStore => "pnpm-old-store",
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
            Self::PnpmOldStore => Some("old pnpm store"),
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
    /// The disk the space comes back to when it is not the scanned one (Docker Desktop keeps
    /// its data in its own virtual disk). Such space stays out of the totals.
    pub elsewhere: Option<&'static str>,
}

impl CleanupCandidate {
    pub fn new(path: PathBuf, kind: CleanupKind, size_bytes: u64) -> Self {
        Self {
            path,
            kind,
            size_bytes,
            elsewhere: None,
        }
    }

    /// Bytes this candidate gives back to the scanned disk.
    pub fn local_bytes(&self) -> u64 {
        if self.elsewhere.is_some() {
            0
        } else {
            self.size_bytes
        }
    }
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub candidates: Vec<CleanupCandidate>,
    pub warnings: Vec<String>,
    pub scanned_entries: u64,
}

impl ScanReport {
    /// Space given back to the scanned disk.
    pub fn total_bytes(&self) -> u64 {
        self.candidates
            .iter()
            .map(CleanupCandidate::local_bytes)
            .sum()
    }

    /// Space given back to other disks, kept out of `total_bytes`.
    fn elsewhere_bytes(&self) -> u64 {
        self.candidates
            .iter()
            .filter(|candidate| candidate.elsewhere.is_some())
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

    let mut report = scan_tree(&root, cutoff_for_old_files(config), spinner);
    scan_global_caches(config, &mut report, spinner);
    Ok(report)
}

fn scan_tree(root: &Path, cutoff: Option<SystemTime>, spinner: &Spinner) -> ScanReport {
    let home = home_dir();
    let in_repo = root
        .ancestors()
        .any(|dir| is_repository(dir, home.as_deref()));
    let mut scanner = Scanner::new(cutoff, home, spinner);
    scanner.scan_dir(root, in_repo);
    scanner
        .report
        .candidates
        .sort_by(|left, right| left.path.cmp(&right.path));
    scanner.report
}

/// A `.git` in the home directory itself is a dotfiles repository; counting it
/// would make every `.bak` in the user's documents look like a project leftover.
fn is_repository(dir: &Path, home: Option<&Path>) -> bool {
    home != Some(dir) && dir.join(".git").exists()
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
            Ok(candidate) if candidate.kind == CleanupKind::PnpmStore => {
                let old_stores = pnpm_old_store_candidates(&candidate.path, &mut report.warnings);
                report.candidates.push(candidate);
                report.candidates.extend(old_stores);
            }
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
    let elsewhere = report.elsewhere_bytes();
    if elsewhere > 0 {
        output.push_str(&format!(
            "Also reclaimable on other disks (not counted above): {}\n",
            format_bytes(elsewhere)
        ));
    }

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
            let disk = candidate
                .elsewhere
                .map(|disk| format!(", on {disk}"))
                .unwrap_or_default();
            output.push_str(&format!(
                "  {} [{:<12}] {} ({}{disk})\n",
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
    home: Option<PathBuf>,
    report: ScanReport,
    spinner: &'a Spinner,
}

impl<'a> Scanner<'a> {
    fn new(cutoff: Option<SystemTime>, home: Option<PathBuf>, spinner: &'a Spinner) -> Self {
        Self {
            cutoff,
            home,
            report: ScanReport::default(),
            spinner,
        }
    }

    fn scan_dir(&mut self, dir: &Path, in_repo: bool) {
        if is_installed_app(dir) {
            return;
        }

        let in_repo = in_repo || is_repository(dir, self.home.as_deref());
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
                if name.as_ref() == "target" {
                    if dir.join("Cargo.toml").is_file() {
                        self.push_dir_candidate(path, CleanupKind::CargoTarget);
                    }
                    continue;
                }

                match project_dir_kind(name.as_ref()) {
                    Some(kind) if self.is_js_project(dir, in_repo) => {
                        self.push_dir_candidate(path, kind);
                        continue;
                    }
                    // Outside a project under development, a `node_modules` holds
                    // global installs, vendored packages or a program's runtime
                    // dependencies: never look inside.
                    Some(CleanupKind::NodeModules) => continue,
                    _ => {}
                }

                if is_skipped_dir(name.as_ref()) {
                    continue;
                }

                self.scan_dir(&path, in_repo);
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

            if is_junk_file(name.as_ref()) || (in_repo && has_temp_extension(&path)) {
                self.report.candidates.push(CleanupCandidate::new(
                    path,
                    CleanupKind::TempFile,
                    metadata.len(),
                ));
                continue;
            }

            if self.matches_old_file(&metadata) {
                self.report.candidates.push(CleanupCandidate::new(
                    path,
                    CleanupKind::OldFile,
                    metadata.len(),
                ));
            }
        }
    }

    /// A `package.json` alone is not enough: installed programs ship one too.
    /// The project must also be in a git repository or have a lockfile in its
    /// directory or in a workspace root above it (the home directory excluded,
    /// where a stray `package-lock.json` would vouch for everything).
    fn is_js_project(&self, dir: &Path, in_repo: bool) -> bool {
        if !dir.join("package.json").is_file() {
            return false;
        }

        in_repo
            || dir
                .ancestors()
                .take_while(|ancestor| self.home.as_deref() != Some(*ancestor))
                .any(has_js_lockfile)
    }

    fn push_dir_candidate(&mut self, path: PathBuf, kind: CleanupKind) {
        self.spinner
            .update(|| (format!("Measuring {kind}"), path.display().to_string()));

        let size_bytes = measure_dir(&path, &mut self.report.warnings);
        self.report
            .candidates
            .push(CleanupCandidate::new(path, kind, size_bytes));
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
        CleanupKind::NodeModules
        | CleanupKind::Dist
        | CleanupKind::NextJs
        | CleanupKind::PnpmOldStore => {
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

    // A failed lookup reads as a native engine: the space is then counted, as before.
    let operating_system = docker
        .run(&["info", "--format", "{{.OperatingSystem}}"], None)
        .unwrap_or_default();

    Ok(CleanupCandidate {
        elsewhere: docker_desktop_disk(&operating_system),
        ..CleanupCandidate::new(
            PathBuf::from("<docker build cache>"),
            CleanupKind::DockerBuildCache,
            size_bytes,
        )
    })
}

/// Docker Desktop (WSL, Windows, macOS) keeps images and build cache in its own virtual disk,
/// so pruning gives the space back there, not to the scanned disk.
fn docker_desktop_disk(operating_system: &str) -> Option<&'static str> {
    operating_system
        .contains("Docker Desktop")
        .then_some("Docker Desktop's virtual disk")
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

    // Entries still hard linked from a project (pnpm's node_modules, uv's virtual environments)
    // are not counted: deleting them gives nothing back, and `pnpm store prune` keeps them.
    let size_bytes = measure_dir(&dir, &mut report.warnings);

    Ok(CleanupCandidate::new(dir, kind, size_bytes))
}

/// Stores of older pnpm versions next to the current one (`v3`, `v10` beside `v11`). pnpm only
/// uses and prunes the current store, so the others are left behind after an upgrade. Files
/// still linked from an old install survive their deletion, so those projects keep working.
fn pnpm_old_store_candidates(current: &Path, warnings: &mut Vec<String>) -> Vec<CleanupCandidate> {
    let (Some(parent), Some(current_name)) = (current.parent(), current.file_name()) else {
        return Vec::new();
    };
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("could not read {}: {error}", parent.display()));
            return Vec::new();
        }
    };

    let mut stores: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| path.file_name() != Some(current_name))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_store_version)
        })
        .collect();
    stores.sort();

    stores
        .into_iter()
        .map(|path| {
            let size_bytes = measure_dir(&path, warnings);
            CleanupCandidate::new(path, CleanupKind::PnpmOldStore, size_bytes)
        })
        .collect()
}

/// pnpm names its store directories after the store format: `v3`, `v10`, `v11`.
fn is_store_version(name: &str) -> bool {
    name.strip_prefix('v').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
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

fn project_dir_kind(name: &str) -> Option<CleanupKind> {
    if name.starts_with(NODE_MODULES_PREFIX) {
        return Some(CleanupKind::NodeModules);
    }

    PROJECT_DIRS
        .iter()
        .find_map(|(dir_name, kind)| (*dir_name == name).then_some(*kind))
}

/// Windows matches names regardless of case, so `appdata` is still `AppData`.
fn is_skipped_dir(name: &str) -> bool {
    name.starts_with('.')
        || SKIPPED_DIRS
            .iter()
            .any(|skipped| skipped.eq_ignore_ascii_case(name))
}

fn is_installed_app(dir: &Path) -> bool {
    ELECTRON_ARCHIVES
        .iter()
        .any(|archive| dir.join(archive).exists())
}

fn has_js_lockfile(dir: &Path) -> bool {
    JS_LOCKFILES
        .iter()
        .any(|lockfile| dir.join(lockfile).is_file())
}

fn is_junk_file(name: &str) -> bool {
    JUNK_FILE_NAMES.contains(&name)
        || JUNK_FILE_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn has_temp_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            TEMP_FILE_EXTENSIONS
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(extension))
        })
}

/// Space that deleting `dir` gives back. A file with several hard links is freed only when its
/// last link goes, so it counts once, and only when every link is inside `dir`: pnpm's
/// node_modules and uv's virtual environments are hard links into their stores.
fn measure_dir(dir: &Path, warnings: &mut Vec<String>) -> u64 {
    measure_tree(dir, &mut HardLinks::default(), warnings)
}

/// Links seen so far of each multiply linked file, by (device, inode).
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Default)]
struct HardLinks(HashMap<(u64, u64), u64>);

impl HardLinks {
    #[cfg(unix)]
    fn reclaimable_len(&mut self, metadata: &fs::Metadata) -> u64 {
        use std::os::unix::fs::MetadataExt;

        let links = metadata.nlink();
        if links <= 1 {
            return metadata.len();
        }
        let seen = self.0.entry((metadata.dev(), metadata.ino())).or_insert(0);
        *seen += 1;
        if *seen == links { metadata.len() } else { 0 }
    }

    // The link count is not exposed by stable std on other platforms.
    #[cfg(not(unix))]
    fn reclaimable_len(&mut self, metadata: &fs::Metadata) -> u64 {
        metadata.len()
    }
}

fn measure_tree(dir: &Path, links: &mut HardLinks, warnings: &mut Vec<String>) -> u64 {
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
                Ok(metadata) => total = total.saturating_add(links.reclaimable_len(&metadata)),
                Err(error) => warnings.push(format!(
                    "could not read metadata for {} while sizing: {error}",
                    path.display()
                )),
            }
            continue;
        }

        if file_type.is_dir() {
            total = total.saturating_add(measure_tree(&path, links, warnings));
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
        CleanupCandidate, CleanupKind, ScanReport, docker_desktop_disk, format_bytes,
        has_temp_extension, is_junk_file, is_repository, parse_docker_size,
        pnpm_old_store_candidates, project_dir_kind, render_report, scan_tree,
    };
    use crate::cli::{Command, Config};
    use crate::progress::Spinner;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn detects_project_directories() {
        assert_eq!(
            project_dir_kind("node_modules"),
            Some(CleanupKind::NodeModules)
        );
        assert_eq!(
            project_dir_kind("node_modules_old"),
            Some(CleanupKind::NodeModules)
        );
        assert_eq!(project_dir_kind("dist"), Some(CleanupKind::Dist));
        assert_eq!(project_dir_kind(".next"), Some(CleanupKind::NextJs));
        assert_eq!(project_dir_kind("src"), None);
    }

    #[test]
    fn detects_junk_and_temp_files() {
        assert!(is_junk_file(".DS_Store"));
        assert!(is_junk_file("Thumbs.db"));
        assert!(is_junk_file("image.png:Zone.Identifier"));
        assert!(!is_junk_file("notes.bak"));

        assert!(has_temp_extension(Path::new("/tmp/file.tmp")));
        assert!(has_temp_extension(Path::new("/tmp/file.TEMP")));
        assert!(!has_temp_extension(Path::new("/tmp/file.ts")));
    }

    #[test]
    fn ignores_a_dotfiles_repository_in_home() {
        let home = std::env::temp_dir().join(format!("kika-home-{}", std::process::id()));
        fs::create_dir_all(home.join(".git")).unwrap();

        assert!(is_repository(&home, None));
        assert!(!is_repository(&home, Some(&home)));

        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn only_matches_leftovers_of_real_projects() {
        let root = std::env::temp_dir().join(format!("kika-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dirs = [
            "app/.git",
            "app/node_modules/react",
            "app/node_modules_old/@firebase/auth/dist",
            "app/dist",
            "app/.next",
            "app/src",
            "vite-only/dist",
            "global/lib/node_modules/typescript/dist",
            "tools/fnm/node-versions/v24.21.0/installation/node_modules/corepack/dist",
            "AppData/Local/Programs/cursor/resources/app/node_modules",
            "appdata/local/tool/node_modules",
            "Programs/cursor/resources/app/node_modules",
            "Programs/dusky/resources/node_modules/@linear",
            ".vscode/extensions/eslint/node_modules",
            "Documents/IRPF",
            "solo/node_modules",
            "mono/packages/ui/node_modules",
            "mono/packages/ui/dist",
        ];
        for dir in dirs {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let files = [
            "app/package.json",
            "app/src/notes.bak",
            "app/node_modules_old/@firebase/auth/package.json",
            "AppData/Local/Programs/cursor/resources/app/package.json",
            "appdata/local/tool/package.json",
            "appdata/local/tool/package-lock.json",
            "Programs/cursor/resources/app/package.json",
            "Programs/dusky/resources/app.asar",
            "Programs/dusky/resources/package.json",
            "Programs/dusky/resources/package-lock.json",
            ".vscode/extensions/eslint/package.json",
            "solo/package.json",
            "solo/package-lock.json",
            "mono/pnpm-lock.yaml",
            "mono/packages/ui/package.json",
            "Documents/IRPF/2014.BAK",
            "Documents/Thumbs.db",
        ];
        for file in files {
            fs::write(root.join(file), "x").unwrap();
        }

        let report = scan_tree(&root, None, &Spinner::hidden());
        let found: Vec<(PathBuf, CleanupKind)> = report
            .candidates
            .into_iter()
            .map(|candidate| {
                let relative = candidate.path.strip_prefix(&root).unwrap().to_path_buf();
                (relative, candidate.kind)
            })
            .collect();

        assert_eq!(
            found,
            vec![
                (PathBuf::from("Documents/Thumbs.db"), CleanupKind::TempFile),
                (PathBuf::from("app/.next"), CleanupKind::NextJs),
                (PathBuf::from("app/dist"), CleanupKind::Dist),
                (PathBuf::from("app/node_modules"), CleanupKind::NodeModules),
                (
                    PathBuf::from("app/node_modules_old"),
                    CleanupKind::NodeModules
                ),
                (PathBuf::from("app/src/notes.bak"), CleanupKind::TempFile),
                (PathBuf::from("mono/packages/ui/dist"), CleanupKind::Dist),
                (
                    PathBuf::from("mono/packages/ui/node_modules"),
                    CleanupKind::NodeModules
                ),
                (PathBuf::from("solo/node_modules"), CleanupKind::NodeModules),
            ]
        );

        fs::remove_dir_all(&root).unwrap();
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
            candidates: vec![CleanupCandidate::new(
                Path::new("/tmp/work/node_modules").to_path_buf(),
                CleanupKind::NodeModules,
                1024,
            )],
            ..Default::default()
        };

        let rendered = render_report(&config, &report);

        assert!(rendered.contains("Mode: report"));
        assert!(rendered.contains("Root: /tmp/work"));
        assert!(rendered.contains("Nothing was deleted. Run `kika clean`"));
    }

    #[test]
    fn keeps_space_on_other_disks_out_of_the_total() {
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
        let docker = CleanupCandidate {
            elsewhere: docker_desktop_disk("Docker Desktop"),
            ..CleanupCandidate::new(
                PathBuf::from("<docker build cache>"),
                CleanupKind::DockerBuildCache,
                5 * 1024 * 1024,
            )
        };
        let report = ScanReport {
            candidates: vec![
                CleanupCandidate::new(
                    PathBuf::from("/tmp/work/node_modules"),
                    CleanupKind::NodeModules,
                    1024,
                ),
                docker,
            ],
            ..Default::default()
        };

        assert_eq!(report.total_bytes(), 1024);
        let rendered = render_report(&config, &report);
        assert!(rendered.contains(&format!(
            "Estimated reclaimable space: {}\n",
            format_bytes(1024)
        )));
        assert!(rendered.contains(&format!(
            "Also reclaimable on other disks (not counted above): {}\n",
            format_bytes(5 * 1024 * 1024)
        )));
        assert!(rendered.contains(", on Docker Desktop's virtual disk)"));
    }

    #[test]
    fn only_docker_desktop_keeps_its_data_on_another_disk() {
        assert_eq!(
            docker_desktop_disk("Docker Desktop"),
            Some("Docker Desktop's virtual disk")
        );
        assert_eq!(docker_desktop_disk("Ubuntu 24.04.3 LTS"), None);
        assert_eq!(docker_desktop_disk(""), None);
    }

    #[cfg(unix)]
    #[test]
    fn counts_a_hard_linked_file_once_and_only_when_every_link_is_inside() {
        use super::measure_dir;

        let root = std::env::temp_dir().join(format!("kika-links-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = root.join("store");
        let modules = root.join("app/node_modules");
        fs::create_dir_all(&store).unwrap();
        fs::create_dir_all(&modules).unwrap();
        // Shared with the store, as pnpm installs a package.
        fs::write(store.join("shared"), vec![0; 1000]).unwrap();
        fs::hard_link(store.join("shared"), modules.join("shared")).unwrap();
        // Only in node_modules.
        fs::write(modules.join("own"), vec![0; 500]).unwrap();
        // Two links, both inside node_modules.
        fs::write(modules.join("twin"), vec![0; 300]).unwrap();
        fs::hard_link(modules.join("twin"), modules.join("twin-link")).unwrap();

        let mut warnings = Vec::new();
        assert_eq!(measure_dir(&modules, &mut warnings), 500 + 300);
        assert_eq!(measure_dir(&store, &mut warnings), 0);
        assert_eq!(measure_dir(&root, &mut warnings), 1000 + 500 + 300);
        assert!(warnings.is_empty());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn finds_the_stores_older_pnpm_versions_left_beside_the_current_one() {
        let root = std::env::temp_dir().join(format!("kika-pnpm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["v3/files", "v10/files", "v11/files", "tmp", "v", "vx1"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("v3/files/a"), vec![0; 30]).unwrap();
        fs::write(root.join("v10/files/a"), vec![0; 100]).unwrap();
        fs::write(root.join("v11/files/a"), vec![0; 110]).unwrap();

        let mut warnings = Vec::new();
        let old = pnpm_old_store_candidates(&root.join("v11"), &mut warnings);

        let found: Vec<(PathBuf, CleanupKind, u64)> = old
            .into_iter()
            .map(|candidate| (candidate.path, candidate.kind, candidate.size_bytes))
            .collect();
        assert_eq!(
            found,
            vec![
                (root.join("v10"), CleanupKind::PnpmOldStore, 100),
                (root.join("v3"), CleanupKind::PnpmOldStore, 30),
            ]
        );
        assert!(warnings.is_empty());

        fs::remove_dir_all(&root).unwrap();
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
