//! 自写的文件系统并行遍历。

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::candidate::classify_candidate_name;
use crate::error::ScanError;
use crate::filter::Filter;
use crate::model::{Candidate, ScanOptions};

/// 默认遍历线程数上限。
pub const DEFAULT_MAX_THREADS: usize = 8;

#[derive(Debug, Default)]
pub struct WalkResult {
    /// 候选文件，已按路径排序，保证输出确定。
    pub candidates: Vec<Candidate>,
    pub dirs_scanned: u64,
}

/// 遍历入口。
pub fn walk(options: &ScanOptions) -> Result<WalkResult, ScanError> {
    let filter = Arc::new(Filter::new(options));
    let roots = resolve_roots(options)?;
    if roots.is_empty() {
        return Err(ScanError::NoRoots);
    }

    let threads = resolve_threads(options.walk_threads);
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            queue: roots.into_iter().collect(),
            pending: 0,
        }),
        cvar: Condvar::new(),
    });
    // 初始 pending = 根目录数量
    {
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending = state.queue.len();
    }

    let dirs_scanned = Arc::new(AtomicU64::new(0));
    let results = Arc::new(Mutex::new(Vec::new()));

    if threads <= 1 {
        let mut local = Vec::new();
        worker_loop(&shared, &filter, &mut local, &dirs_scanned);
        push_results(&results, local);
    } else {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .map_err(|e| ScanError::ThreadPool(e.to_string()))?;
        pool.install(|| {
            rayon::scope(|scope| {
                for _ in 0..threads {
                    let shared = Arc::clone(&shared);
                    let filter = Arc::clone(&filter);
                    let dirs_scanned = Arc::clone(&dirs_scanned);
                    let results = Arc::clone(&results);
                    scope.spawn(move |_| {
                        let mut local = Vec::new();
                        worker_loop(&shared, &filter, &mut local, &dirs_scanned);
                        push_results(&results, local);
                    });
                }
            });
        });
    }

    let mut candidates = results.lock().unwrap_or_else(|e| e.into_inner()).clone();
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(WalkResult {
        candidates,
        dirs_scanned: dirs_scanned.load(Ordering::Relaxed),
    })
}

fn push_results(results: &Mutex<Vec<Candidate>>, local: Vec<Candidate>) {
    if local.is_empty() {
        return;
    }
    let mut guard = results.lock().unwrap_or_else(|e| e.into_inner());
    guard.extend(local);
}

struct Shared {
    state: Mutex<State>,
    cvar: Condvar,
}

struct State {
    queue: VecDeque<PathBuf>,
    /// 队列中待处理的目录数 + 正在被 worker 处理的目录数。
    pending: usize,
}

fn worker_loop(
    shared: &Shared,
    filter: &Filter,
    local: &mut Vec<Candidate>,
    dirs_scanned: &AtomicU64,
) {
    let mut guard = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let next = guard.queue.pop_front();
        if let Some(dir) = next {
            drop(guard);

            let subdirs = scan_dir(&dir, filter, local);
            dirs_scanned.fetch_add(1, Ordering::Relaxed);

            guard = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if !subdirs.is_empty() {
                guard.pending += subdirs.len();
                guard.queue.extend(subdirs);
                shared.cvar.notify_all();
            }
            guard.pending -= 1;
            if guard.pending == 0 {
                shared.cvar.notify_all();
            }
            continue;
        }

        if guard.pending == 0 {
            break;
        }
        guard = shared
            .cvar
            .wait_timeout(guard, std::time::Duration::from_millis(1))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// 读取单个目录：收集子目录、挑出候选文件。
fn scan_dir(dir: &Path, filter: &Filter, local: &mut Vec<Candidate>) -> Vec<PathBuf> {
    let mut subdirs = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return subdirs;
    };

    for entry in entries.flatten() {
        // `file_type()` 不跟随符号链接。
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            let path = entry.path();
            if filter.allows_dir(&path) {
                subdirs.push(path);
            }
            continue;
        }

        if file_type.is_file() {
            let name: OsString = entry.file_name();
            if let Some(kind) = classify_candidate_name(&name) {
                local.push(Candidate {
                    path: entry.path(),
                    kind,
                });
            }
            continue;
        }

        if file_type.is_symlink() && filter.follow_symlinks() {
            let path = entry.path();
            if let Ok(metadata) = fs::metadata(&path)
                && metadata.is_dir()
                && filter.allows_dir(&path)
            {
                subdirs.push(path);
            }
        }
    }

    subdirs
}

fn resolve_threads(configured: usize) -> usize {
    if configured > 0 {
        return configured;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .min(DEFAULT_MAX_THREADS)
}

/// 未指定 root 时推导默认起点。
fn resolve_roots(options: &ScanOptions) -> Result<Vec<PathBuf>, ScanError> {
    if !options.roots.is_empty() {
        return Ok(options
            .roots
            .iter()
            .map(|root| normalize_root(root.as_path()))
            .collect());
    }
    default_roots()
}

/// 统一分隔符。
#[cfg(target_os = "windows")]
fn normalize_root(root: &Path) -> PathBuf {
    let text = root.to_string_lossy().replace('/', "\\");
    PathBuf::from(text)
}

#[cfg(not(target_os = "windows"))]
fn normalize_root(root: &Path) -> PathBuf {
    root.to_path_buf()
}

#[cfg(target_os = "windows")]
fn default_roots() -> Result<Vec<PathBuf>, ScanError> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;

    // SAFETY: GetLogicalDrives 没有参数，返回驱动器位掩码。
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err(ScanError::io(
            PathBuf::from("<drives>"),
            std::io::Error::last_os_error(),
        ));
    }
    Ok((0..26u8)
        .filter(|index| mask & (1u32 << *index) != 0)
        .map(|index| PathBuf::from(format!("{}:\\", (b'A' + index) as char)))
        .collect())
}

#[cfg(not(target_os = "windows"))]
fn default_roots() -> Result<Vec<PathBuf>, ScanError> {
    Ok(vec![PathBuf::from("/")])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CandidateKind;
    use std::fs;

    struct Fixture {
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fixture(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "cefscan-walk-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn options_for(root: &Path, threads: usize) -> ScanOptions {
        ScanOptions {
            roots: vec![root.to_path_buf()],
            walk_threads: threads,
            exclude_dir_names: vec!["skipme".into()],
            ..ScanOptions::default()
        }
    }

    #[test]
    fn finds_candidates_and_sorts_deterministically() {
        let fixture = fixture("sort");
        for dir in ["b", "a", "nested/deep"] {
            fs::create_dir_all(fixture.root.join(dir)).unwrap();
            fs::write(
                fixture.root.join(dir).join(if cfg!(windows) {
                    "libcef.dll"
                } else {
                    "libcef.so"
                }),
                b"data",
            )
            .unwrap();
        }

        let found = walk(&options_for(&fixture.root, 4)).unwrap();
        let paths: Vec<_> = found.candidates.iter().map(|c| c.path.clone()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "结果必须按路径排序");
        assert_eq!(found.candidates.len(), 3);
        assert!(
            found
                .candidates
                .iter()
                .all(|c| c.kind == CandidateKind::Cef)
        );
    }

    #[test]
    fn pruned_subtrees_are_never_entered() {
        let fixture = fixture("prune");
        fs::create_dir_all(fixture.root.join("skipme")).unwrap();
        fs::write(fixture.root.join("skipme").join("libcef.dll"), b"data").unwrap();
        fs::write(fixture.root.join("libcef.dll"), b"data").unwrap();

        let found = walk(&options_for(&fixture.root, 4)).unwrap();
        assert_eq!(found.candidates.len(), 1);
        assert!(found.candidates[0].path.ends_with("libcef.dll"));
        assert!(
            !found.candidates[0]
                .path
                .components()
                .any(|c| c.as_os_str() == "skipme")
        );
    }

    #[test]
    fn pak_and_node_names_are_classified_during_the_walk() {
        let fixture = fixture("kinds");
        fs::write(fixture.root.join("chrome_100_percent.pak"), b"x").unwrap();
        fs::write(fixture.root.join("libnode.dll"), b"x").unwrap();
        fs::write(fixture.root.join("unrelated.txt"), b"x").unwrap();

        let found = walk(&options_for(&fixture.root, 2)).unwrap();
        let kinds: Vec<_> = found.candidates.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&CandidateKind::Pak));
        assert!(kinds.contains(&CandidateKind::Node));
        assert_eq!(found.candidates.len(), 2);
    }

    #[test]
    fn every_thread_count_produces_the_same_result() {
        let fixture = fixture("threads");
        for index in 0..40 {
            let dir = fixture.root.join(format!("app{index}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("libcef.dll"), b"data").unwrap();
        }

        let single = walk(&options_for(&fixture.root, 1)).unwrap();
        for threads in [2usize, 3, 8, 16] {
            let parallel = walk(&options_for(&fixture.root, threads)).unwrap();
            assert_eq!(
                single.candidates, parallel.candidates,
                "threads={threads} 的结果与单线程不一致"
            );
            assert_eq!(single.dirs_scanned, parallel.dirs_scanned);
        }
        assert_eq!(single.dirs_scanned, 41, "40 个子目录 + 根");
    }

    #[test]
    fn empty_roots_fall_back_to_platform_defaults() {
        // 不指定 root 时走平台默认起点：Windows 是各个盘符，Unix 是 `/`。
        let roots = resolve_roots(&ScanOptions::default()).unwrap();
        assert!(!roots.is_empty());
        assert!(roots.iter().all(|root| root.is_absolute()), "{roots:?}");
    }

    #[test]
    fn explicit_roots_are_used_as_given() {
        // 显式 root 原样采用；Windows 上只多做一次分隔符归一化（`/` -> `\`）。
        let given = if cfg!(windows) { r"C:\apps" } else { "/apps" };
        let options = ScanOptions {
            roots: vec![PathBuf::from(given)],
            ..ScanOptions::default()
        };
        assert_eq!(resolve_roots(&options).unwrap(), vec![PathBuf::from(given)]);
    }
}
