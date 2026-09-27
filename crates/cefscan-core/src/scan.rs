//! 扫描编排：遍历 → 归并 → 计量 → 运行态。

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use crate::error::ScanError;
use crate::group::{DetectedApp, group};
use crate::model::{AppInfo, Backend, Candidate, ScanOptions, ScanStats};
use crate::process::{self, ProcessKey};
use crate::size::sizes_parallel_each;
use crate::walk::walk;

#[cfg(all(feature = "everything", target_os = "windows"))]
mod everything;

/// 一次扫描的完整结果。
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    pub apps: Vec<AppInfo>,
    pub stats: ScanStats,
}

/// 执行一次扫描，返回**已排序**的结果。
///
/// # Errors
///
/// 只有在没有任何可遍历的根、或索引后端与文件系统后端同时失败时才返回错误；
/// 单个文件读不到、单个目录没权限都只会降级，不会中断扫描。
pub fn scan(options: &ScanOptions) -> Result<ScanOutcome, ScanError> {
    let mut apps = Vec::new();
    let stats = scan_streaming(options, |app| apps.push(app))?;
    sort_apps(&mut apps, options.sort_by_size);
    Ok(ScanOutcome { apps, stats })
}

/// 把结果排成确定性顺序：默认按占用降序，否则按路径升序。
///
/// 放在这里而不是 `scan_streaming` 里，是因为流式模式下结果是一个个蹦出来的，
/// 没法在发射前排序——调用方（比如 CLI）收完自己排一次即可。
pub fn sort_apps(apps: &mut [AppInfo], by_size: bool) {
    if by_size {
        apps.sort_by(|left, right| {
            right
                .size
                .cmp(&left.size)
                .then_with(|| left.path.cmp(&right.path))
        });
    } else {
        apps.sort_by(|left, right| left.path.cmp(&right.path));
    }
}

/// 流式扫描：每识别出一个应用就回调一次，**边算边回调**。
///
/// GUI 用它做渐进式渲染：小的应用先出现在列表里，大的随后补上，
/// 而不是等最慢的那个目录计量完才一次性刷出全部结果。
/// CLI 用 `scan()` 收集成 `Vec` 再排序。两者共用同一条流水线，
/// 不存在"GUI 结果和 CLI 结果不一致"的可能。
pub fn scan_streaming<F>(options: &ScanOptions, on_app: F) -> Result<ScanStats, ScanError>
where
    F: FnMut(AppInfo) + Send,
{
    let started = Instant::now();

    let running: HashSet<ProcessKey> = if options.detect_running {
        process::running_processes()
    } else {
        HashSet::new()
    };

    let (candidates, backend_name, dirs_scanned) = discover(options)?;
    let threads = resolve_scan_threads(options.scan_threads);

    let detected = group(&candidates, threads);
    let roots: Vec<PathBuf> = detected.iter().map(|app| app.root.clone()).collect();

    // 计量是并行跑的，所以回调要加锁；用 Mutex<FnMut> 而不是给回调加 Send+Sync
    // 约束，是为了让 `|app| apps.push(app)` 这种最朴素的收集写法也能用。
    let on_app = Mutex::new(on_app);
    let collected: Mutex<Vec<AppInfo>> = Mutex::new(Vec::with_capacity(detected.len()));

    sizes_parallel_each(&roots, threads, |index, _path, size| {
        let app = &detected[index];
        let is_running = app
            .executable
            .as_deref()
            .is_some_and(|path| process::is_running(&running, path));
        let info = to_app_info(app, is_running, size);

        (on_app.lock().expect("callback poisoned"))(info.clone());
        collected.lock().expect("collector poisoned").push(info);
    });

    let apps = collected
        .into_inner()
        .unwrap_or_else(|error| error.into_inner());

    let sum_bytes = apps.iter().map(|app| app.size).sum();
    let total_bytes = deduplicated_total(&apps);
    let stats = ScanStats {
        backend: backend_name,
        dirs_scanned,
        candidates: candidates.len(),
        apps: apps.len(),
        sum_bytes,
        total_bytes,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };

    Ok(stats)
}

fn to_app_info(app: &DetectedApp, running: bool, size: u64) -> AppInfo {
    AppInfo {
        path: app.display.clone(),
        root: app.root.clone(),
        kind: app.kind,
        size,
        running,
        evidence: app.evidence,
    }
}

/// 去重口径的总量：被其它根包含的目录不重复计入。
fn deduplicated_total(apps: &[AppInfo]) -> u64 {
    let mut total = 0_u64;
    for app in apps {
        if apps.iter().any(|other| {
            other.root != app.root
                && crate::filter::path_starts_with(&app.root, &other.root)
        }) {
            continue;
        }
        total = total.saturating_add(app.size);
    }
    total
}

/// 取得候选文件。索引后端失败时按策略回落。
fn discover(options: &ScanOptions) -> Result<(Vec<Candidate>, &'static str, u64), ScanError> {
    match options.backend {
        Backend::Filesystem => {
            let result = walk(options)?;
            Ok((result.candidates, "filesystem", result.dirs_scanned))
        }
        Backend::Index => match try_index_candidates(options) {
            Ok(candidates) => Ok((candidates, "index", 0)),
            Err(error) => Err(ScanError::BothBackendsFailed {
                index: error.to_string(),
                fallback: "fallback disabled by --backend index".into(),
            }),
        },
        Backend::Auto => match try_index_candidates(options) {
            Ok(candidates) => Ok((candidates, "index", 0)),
            Err(_index_error) => {
                let result = walk(options)?;
                Ok((result.candidates, "filesystem", result.dirs_scanned))
            }
        },
    }
}

#[cfg(all(feature = "everything", target_os = "windows"))]
fn try_index_candidates(options: &ScanOptions) -> io::Result<Vec<Candidate>> {
    everything::query_candidates(options)
}

#[cfg(not(all(feature = "everything", target_os = "windows")))]
fn try_index_candidates(_options: &ScanOptions) -> io::Result<Vec<Candidate>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the index backend requires Windows and the `everything` feature",
    ))
}

fn resolve_scan_threads(configured: usize) -> usize {
    if configured > 0 {
        return configured;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
}
