//! 把候选文件归并成"应用"。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::inspect::{DirInspection, inspect_directory};
use crate::model::{AppKind, Candidate, CandidateKind};
use crate::signature::{Flavor, SignatureScanner};

#[derive(Debug, Clone)]
pub struct DetectedApp {
    /// 展示路径：可执行文件优先，其次是应用根目录。
    pub display: PathBuf,
    pub executable: Option<PathBuf>,
    pub kind: AppKind,
    pub evidence: Option<&'static str>,
    /// 计量与去重所用的根目录。
    pub root: PathBuf,
    /// 是否只能以目录形式展示。
    pub is_dir: bool,
}

#[derive(Default, Clone, Copy)]
struct Flags {
    pak: bool,
    cef: bool,
}

pub fn group(candidates: &[Candidate], threads: usize) -> Vec<DetectedApp> {
    let mut standard_dirs: BTreeMap<PathBuf, Flags> = BTreeMap::new();
    let mut node_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for candidate in candidates {
        let Some(dir) = candidate.path.parent() else {
            continue;
        };
        match candidate.kind {
            CandidateKind::Pak => standard_dirs.entry(dir.to_path_buf()).or_default().pak = true,
            CandidateKind::Cef => standard_dirs.entry(dir.to_path_buf()).or_default().cef = true,
            CandidateKind::Node => {
                node_dirs.insert(dir.to_path_buf());
            }
        }
    }

    let dirs: Vec<PathBuf> = standard_dirs.keys().cloned().collect();
    let mut inspections = inspect_parallel(&dirs, Flavor::Standard, threads);

    // 本目录什么都没查到时，退到父目录再试一次。
    let parents: Vec<PathBuf> = dirs
        .iter()
        .filter(|dir| {
            inspections
                .get(dir.as_path())
                .is_none_or(DirInspection::is_empty_ref)
        })
        .filter_map(|dir| dir.parent())
        .map(Path::to_path_buf)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !parents.is_empty() {
        for (path, inspection) in inspect_parallel(&parents, Flavor::Standard, threads) {
            inspections.entry(path).or_insert(inspection);
        }
    }

    let mut apps: BTreeMap<PathBuf, DetectedApp> = BTreeMap::new();
    for dir in &dirs {
        // 回退到父目录时，root 也要跟着迁到父目录。
        let (root, inspection) = inspection_for(&inspections, dir);
        let default_kind = if standard_dirs.get(dir).is_some_and(|flags| flags.cef) {
            AppKind::Cef
        } else {
            AppKind::Unknown
        };
        insert(&mut apps, build(&root, &inspection, default_kind));
    }

    let node_dirs: Vec<PathBuf> = node_dirs.into_iter().collect();
    if !node_dirs.is_empty() {
        for (dir, inspection) in inspect_parallel(&node_dirs, Flavor::Mini, threads) {
            let Some(kind) = inspection.kind else {
                continue;
            };
            insert(&mut apps, build(&dir, &inspection, kind));
        }
    }

    drop_apps_nested_in_identified_roots(&mut apps);
    apps.into_values().collect()
}

impl DirInspection {
    fn is_empty_ref(&self) -> bool {
        self.kind.is_none() && self.executable.is_none()
    }
}

/// 取目录自身的检查结果；自身为空时退到父目录，并连 root 一起返回父目录。
fn inspection_for(
    inspections: &HashMap<PathBuf, DirInspection>,
    dir: &Path,
) -> (PathBuf, DirInspection) {
    let own = inspections.get(dir).cloned().unwrap_or_default();
    if !own.is_empty_ref() {
        return (dir.to_path_buf(), own);
    }
    match dir.parent().and_then(|parent| inspections.get(parent)) {
        Some(parent) if !parent.is_empty_ref() => {
            (dir.parent().unwrap_or(dir).to_path_buf(), parent.clone())
        }
        _ => (dir.to_path_buf(), own),
    }
}

fn build(dir: &Path, inspection: &DirInspection, default_kind: AppKind) -> DetectedApp {
    let kind = inspection.kind.unwrap_or(default_kind);
    match inspection.executable.clone() {
        Some(executable) => DetectedApp {
            display: executable.clone(),
            executable: Some(executable),
            kind,
            evidence: inspection.evidence,
            root: dir.to_path_buf(),
            is_dir: false,
        },
        None => DetectedApp {
            display: dir.to_path_buf(),
            executable: None,
            kind,
            evidence: inspection.evidence,
            root: dir.to_path_buf(),
            is_dir: true,
        },
    }
}

/// 同一 root 只保留最强的一条。
fn insert(apps: &mut BTreeMap<PathBuf, DetectedApp>, detected: DetectedApp) {
    if apps
        .get(&detected.root)
        .is_some_and(|existing| beats(existing, &detected))
    {
        return;
    }
    apps.insert(detected.root.clone(), detected);
}

/// `existing` 是否该压过 `candidate`：先比 rank；rank 相同时「有可执行文件」胜过「纯目录」。
fn beats(existing: &DetectedApp, candidate: &DetectedApp) -> bool {
    let existing_rank = existing.kind.rank();
    let candidate_rank = candidate.kind.rank();
    existing_rank > candidate_rank
        || (existing_rank == candidate_rank && !existing.is_dir && candidate.is_dir)
}

/// 丢掉"未识别但被某个已识别应用包含"的目录。
fn drop_apps_nested_in_identified_roots(apps: &mut BTreeMap<PathBuf, DetectedApp>) {
    let identified: Vec<PathBuf> = apps
        .values()
        .filter(|app| app.kind != AppKind::Unknown)
        .map(|app| app.root.clone())
        .collect();
    let stale: Vec<PathBuf> = apps
        .values()
        .filter(|app| app.kind == AppKind::Unknown)
        .filter(|app| {
            identified
                .iter()
                .any(|root| root != &app.root && crate::filter::path_starts_with(&app.root, root))
        })
        .map(|app| app.root.clone())
        .collect();
    for root in stale {
        apps.remove(&root);
    }
}

/// 并行检查一批目录。
fn inspect_parallel(
    dirs: &[PathBuf],
    flavor: Flavor,
    threads: usize,
) -> HashMap<PathBuf, DirInspection> {
    use rayon::prelude::*;

    if dirs.is_empty() {
        return HashMap::new();
    }

    let run = || {
        dirs.par_iter()
            .map_init(SignatureScanner::new, |scanner, dir| {
                (dir.clone(), inspect_directory(dir, flavor, scanner))
            })
            .collect::<HashMap<_, _>>()
    };

    if threads <= 1 {
        run()
    } else {
        match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
            Ok(pool) => pool.install(run),
            Err(_) => run(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str, kind: CandidateKind) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            kind,
        }
    }

    #[test]
    fn one_directory_with_several_candidates_collapses_to_one_app() {
        let apps = group(
            &[
                candidate("/apps/demo/libcef.dll", CandidateKind::Cef),
                candidate("/apps/demo/chrome_100_percent.pak", CandidateKind::Pak),
            ],
            2,
        );
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].root, PathBuf::from("/apps/demo"));
    }

    #[test]
    fn cef_candidates_default_to_cef_and_pak_to_unknown() {
        let apps = group(&[candidate("/apps/cef/libcef.dll", CandidateKind::Cef)], 1);
        assert_eq!(apps[0].kind, AppKind::Cef);

        let apps = group(
            &[candidate(
                "/apps/pak/chrome_100_percent.pak",
                CandidateKind::Pak,
            )],
            1,
        );
        assert_eq!(apps[0].kind, AppKind::Unknown);
    }

    #[test]
    fn unknown_roots_nested_in_identified_roots_are_dropped() {
        let mut apps: BTreeMap<PathBuf, DetectedApp> = BTreeMap::new();
        apps.insert(
            PathBuf::from("/apps/demo"),
            DetectedApp {
                display: PathBuf::from("/apps/demo/app.exe"),
                executable: Some(PathBuf::from("/apps/demo/app.exe")),
                kind: AppKind::Electron,
                evidence: None,
                root: PathBuf::from("/apps/demo"),
                is_dir: false,
            },
        );
        apps.insert(
            PathBuf::from("/apps/demo/resources"),
            DetectedApp {
                display: PathBuf::from("/apps/demo/resources"),
                executable: None,
                kind: AppKind::Unknown,
                evidence: None,
                root: PathBuf::from("/apps/demo/resources"),
                is_dir: true,
            },
        );
        drop_apps_nested_in_identified_roots(&mut apps);
        assert_eq!(apps.len(), 1);
        assert!(apps.contains_key(&PathBuf::from("/apps/demo")));
    }

    #[test]
    fn stronger_kinds_win_for_the_same_root() {
        let mut apps: BTreeMap<PathBuf, DetectedApp> = BTreeMap::new();
        let root = PathBuf::from("/apps/x");
        insert(
            &mut apps,
            DetectedApp {
                display: root.clone(),
                executable: None,
                kind: AppKind::Cef,
                evidence: None,
                root: root.clone(),
                is_dir: true,
            },
        );
        insert(
            &mut apps,
            DetectedApp {
                display: root.join("app.exe"),
                executable: Some(root.join("app.exe")),
                kind: AppKind::Electron,
                evidence: None,
                root: root.clone(),
                is_dir: false,
            },
        );
        assert_eq!(apps[&root].kind, AppKind::Electron);
    }
}
