use std::cmp::Reverse;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

/// Tools installed per Node.js version, which fnm/nvm only put on `PATH` inside
/// an initialized shell (cron jobs and non-interactive shells won't see them).
const NODE_TOOLS: &[&str] = &["npm", "pnpm", "yarn"];

/// An external program resolved to an executable path.
#[derive(Debug)]
pub struct Tool {
    name: &'static str,
    program: PathBuf,
    /// Directory prepended to the child's `PATH`. Node tools are scripts with a
    /// `#!/usr/bin/env node` shebang, so their Node.js `bin` must be reachable.
    path_prefix: Option<PathBuf>,
}

impl Tool {
    pub fn find(name: &'static str) -> io::Result<Self> {
        if let Some(program) = env::var_os("PATH").and_then(|path| find_in_dirs(name, &path)) {
            return Ok(Self {
                name,
                program,
                path_prefix: None,
            });
        }

        if NODE_TOOLS.contains(&name) {
            for bin_dir in node_manager_bin_dirs() {
                let program = bin_dir.join(name);
                if program.is_file() {
                    return Ok(Self {
                        name,
                        program,
                        path_prefix: Some(bin_dir),
                    });
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{name} not found in PATH"),
        ))
    }

    /// Runs the tool and returns its stdout, turning a non-zero exit into an error.
    pub fn run(&self, args: &[&str], current_dir: Option<&Path>) -> io::Result<String> {
        let mut command = process::Command::new(&self.program);
        command.args(args);

        if let Some(dir) = current_dir {
            command.current_dir(dir);
        }

        if let Some(prefix) = &self.path_prefix {
            command.env("PATH", prepend_path(prefix)?);
        }

        let output = command.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Node tools dump whole stack traces; the first line carries the message.
            let message = stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("no stderr output");
            let location = current_dir
                .map(|dir| format!(" in {}", dir.display()))
                .unwrap_or_default();
            return Err(io::Error::other(format!(
                "`{} {}` failed{location}: {message}",
                self.name,
                args.join(" "),
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

fn find_in_dirs(name: &str, path: &OsString) -> Option<PathBuf> {
    env::split_paths(path).find_map(|dir| {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }

        let candidate = candidate.with_extension("exe");
        candidate.is_file().then_some(candidate)
    })
}

fn prepend_path(prefix: &Path) -> io::Result<OsString> {
    let current = env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(prefix.to_path_buf()).chain(env::split_paths(&current));
    env::join_paths(dirs).map_err(io::Error::other)
}

/// Node.js `bin` directories from fnm and nvm, most preferred first: each
/// manager's default alias, then its installed versions from newest to oldest.
fn node_manager_bin_dirs() -> Vec<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();

    for fnm_dir in fnm_dirs(home.as_deref()) {
        dirs.push(fnm_dir.join("aliases").join("default").join("bin"));
        dirs.extend(
            sorted_versions(&fnm_dir.join("node-versions"))
                .into_iter()
                .map(|version| version.join("installation").join("bin")),
        );
    }

    for nvm_dir in nvm_dirs(home.as_deref()) {
        let versions = sorted_versions(&nvm_dir.join("versions").join("node"));
        let default = fs::read_to_string(nvm_dir.join("alias").join("default")).ok();

        if let Some(version) = default.and_then(|alias| match_nvm_alias(&versions, &alias)) {
            dirs.push(version.join("bin"));
        }

        dirs.extend(versions.into_iter().map(|version| version.join("bin")));
    }

    dirs.retain(|dir| dir.join("node").is_file());
    dirs.dedup();
    dirs
}

fn fnm_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env::var_os("FNM_DIR")
        .map(PathBuf::from)
        .into_iter()
        .collect();

    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data_home).join("fnm"));
    }

    if let Some(home) = home {
        dirs.push(home.join(".local").join("share").join("fnm"));
        dirs.push(home.join(".fnm"));
    }

    dirs
}

fn nvm_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    dirs.extend(home.map(|home| home.join(".nvm")));
    dirs
}

/// Version directories (`v22.14.0`, ...) under `dir`, newest first.
fn sorted_versions(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut versions: Vec<(Vec<u64>, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let version = parse_version(&entry.file_name().to_string_lossy())?;
            Some((version, entry.path()))
        })
        .collect();

    versions.sort_by_key(|(version, _)| Reverse(version.clone()));
    versions.into_iter().map(|(_, path)| path).collect()
}

fn parse_version(name: &str) -> Option<Vec<u64>> {
    name.strip_prefix('v')?
        .split('.')
        .map(|part| part.parse().ok())
        .collect()
}

/// Resolves an nvm default alias such as `22`, `v22.14.0` or `node` against the
/// installed versions (newest first). Named aliases like `lts/*` are not resolved.
fn match_nvm_alias(versions: &[PathBuf], alias: &str) -> Option<PathBuf> {
    let alias = alias.trim();

    if alias == "node" || alias == "stable" {
        return versions.first().cloned();
    }

    let wanted = parse_version(&format!("v{}", alias.trim_start_matches('v')))?;

    versions
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| parse_version(&name.to_string_lossy()))
                .is_some_and(|version| version.starts_with(&wanted))
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::{match_nvm_alias, parse_version, sorted_versions};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn parses_node_versions() {
        assert_eq!(parse_version("v22.14.0"), Some(vec![22, 14, 0]));
        assert_eq!(parse_version("22.14.0"), None);
        assert_eq!(parse_version("vfoo"), None);
    }

    #[test]
    fn matches_nvm_aliases() {
        let versions = vec![
            PathBuf::from("/nvm/versions/node/v24.1.0"),
            PathBuf::from("/nvm/versions/node/v22.14.0"),
            PathBuf::from("/nvm/versions/node/v22.2.0"),
        ];

        assert_eq!(
            match_nvm_alias(&versions, "22\n"),
            Some(PathBuf::from("/nvm/versions/node/v22.14.0"))
        );
        assert_eq!(
            match_nvm_alias(&versions, "v22.2.0"),
            Some(PathBuf::from("/nvm/versions/node/v22.2.0"))
        );
        assert_eq!(
            match_nvm_alias(&versions, "node"),
            Some(PathBuf::from("/nvm/versions/node/v24.1.0"))
        );
        assert_eq!(match_nvm_alias(&versions, "20"), None);
        assert_eq!(match_nvm_alias(&versions, "lts/iron"), None);
    }

    #[test]
    fn sorts_versions_numerically_newest_first() {
        let dir = env_temp_dir("sorts_versions");
        for name in ["v9.0.0", "v18.20.8", "v18.3.0", "not-a-version"] {
            fs::create_dir_all(dir.join(name)).unwrap();
        }

        let names: Vec<String> = sorted_versions(&dir)
            .into_iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(names, vec!["v18.20.8", "v18.3.0", "v9.0.0"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    fn env_temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kika-tools-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }
}
