use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::cleanup::{CleanupCandidate, CleanupKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// Candidates under the same git repository root.
    Repository,
    /// Candidates outside any repository, grouped by their project directory.
    Directory,
    /// A single global cache (Docker, pnpm, uv, yarn, npm).
    GlobalCache,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub label: String,
    pub kind: GroupKind,
    pub candidates: Vec<CleanupCandidate>,
}

impl Group {
    pub fn size_bytes(&self) -> u64 {
        self.candidates
            .iter()
            .map(|candidate| candidate.size_bytes)
            .sum()
    }

    /// Distinct kinds in this group, in a stable order.
    pub fn kinds(&self) -> Vec<CleanupKind> {
        let mut kinds: Vec<CleanupKind> = self
            .candidates
            .iter()
            .map(|candidate| candidate.kind)
            .collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }
}

/// Groups candidates by git repository (or project directory when outside a
/// repository), largest first, followed by the global caches.
pub fn group_candidates(root: &Path, candidates: Vec<CleanupCandidate>) -> Vec<Group> {
    let mut projects: BTreeMap<PathBuf, (GroupKind, Vec<CleanupCandidate>)> = BTreeMap::new();
    let mut caches = Vec::new();
    let mut repo_roots = HashMap::new();

    for candidate in candidates {
        if let Some(name) = candidate.kind.global_cache_name() {
            caches.push(Group {
                label: name.to_string(),
                kind: GroupKind::GlobalCache,
                candidates: vec![candidate],
            });
            continue;
        }

        let project_dir = candidate
            .path
            .parent()
            .unwrap_or(&candidate.path)
            .to_path_buf();
        let (key, kind) = match find_repo_root(root, &project_dir, &mut repo_roots) {
            Some(repo) => (repo, GroupKind::Repository),
            None => (project_dir, GroupKind::Directory),
        };

        projects
            .entry(key)
            .or_insert_with(|| (kind, Vec::new()))
            .1
            .push(candidate);
    }

    let mut groups: Vec<Group> = projects
        .into_iter()
        .map(|(dir, (kind, candidates))| Group {
            label: relative_label(root, &dir),
            kind,
            candidates,
        })
        .collect();

    groups.sort_by_key(|group| std::cmp::Reverse(group.size_bytes()));
    caches.sort_by_key(|group| std::cmp::Reverse(group.size_bytes()));
    groups.extend(caches);
    groups
}

/// Nearest directory with a `.git` entry, without leaving the scanned root, so
/// a repository wrapping the whole tree (e.g. a dotfiles repo in `$HOME`) does
/// not swallow every project.
fn find_repo_root(
    root: &Path,
    start: &Path,
    memo: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Option<PathBuf> {
    if let Some(found) = memo.get(start) {
        return found.clone();
    }

    let found = start
        .ancestors()
        .take_while(|dir| dir.starts_with(root))
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf);

    memo.insert(start.to_path_buf(), found.clone());
    found
}

fn relative_label(root: &Path, dir: &Path) -> String {
    match dir.strip_prefix(root) {
        Ok(relative) if relative.as_os_str().is_empty() => root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
        Ok(relative) => relative.display().to_string(),
        Err(_) => dir.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{GroupKind, group_candidates};
    use crate::cleanup::{CleanupCandidate, CleanupKind};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn candidate(path: PathBuf, kind: CleanupKind, size_bytes: u64) -> CleanupCandidate {
        CleanupCandidate {
            path,
            kind,
            size_bytes,
        }
    }

    #[test]
    fn groups_by_repository_then_directory_then_cache() {
        let root = std::env::temp_dir().join(format!("kika-groups-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("app/.git")).unwrap();
        fs::create_dir_all(root.join("app/web/node_modules")).unwrap();
        fs::create_dir_all(root.join("loose/node_modules")).unwrap();

        let groups = group_candidates(
            &root,
            vec![
                candidate(root.join("app/node_modules"), CleanupKind::NodeModules, 10),
                candidate(
                    root.join("app/web/node_modules"),
                    CleanupKind::NodeModules,
                    20,
                ),
                candidate(root.join("app/web/notes.tmp"), CleanupKind::TempFile, 1),
                candidate(root.join("loose/node_modules"), CleanupKind::NodeModules, 5),
                candidate(PathBuf::from("/cache/uv"), CleanupKind::UvCache, 500),
            ],
        );

        let summary: Vec<(&str, GroupKind, usize, u64)> = groups
            .iter()
            .map(|group| {
                (
                    group.label.as_str(),
                    group.kind,
                    group.candidates.len(),
                    group.size_bytes(),
                )
            })
            .collect();

        assert_eq!(
            summary,
            vec![
                ("app", GroupKind::Repository, 3, 31),
                ("loose", GroupKind::Directory, 1, 5),
                ("uv cache", GroupKind::GlobalCache, 1, 500),
            ]
        );
        assert_eq!(
            groups[0].kinds(),
            vec![CleanupKind::NodeModules, CleanupKind::TempFile]
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ignores_repositories_above_the_scanned_root() {
        let base = std::env::temp_dir().join(format!("kika-groups-up-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("projects");
        fs::create_dir_all(base.join(".git")).unwrap();
        fs::create_dir_all(root.join("site/dist")).unwrap();

        let groups = group_candidates(
            &root,
            vec![candidate(root.join("site/dist"), CleanupKind::Dist, 3)],
        );

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "site");
        assert_eq!(groups[0].kind, GroupKind::Directory);

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn labels_the_root_itself_by_name() {
        let root = Path::new("/work/kika");
        let groups = group_candidates(
            root,
            vec![candidate(root.join("dist"), CleanupKind::Dist, 1)],
        );

        assert_eq!(groups[0].label, "kika");
    }
}
