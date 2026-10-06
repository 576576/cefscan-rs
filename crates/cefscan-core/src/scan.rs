//! 扫描编排：遍历 → 归并 → 计量 → 运行态。

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use crate::error::ScanError;
use crate::group::{DetectedApp, group};
use crate::model::{
    AppInfo, Backend, Candidate, Direction, FILESYSTEM_BACKEND, ScanNotice, ScanOptions, ScanStats,
    SortKey,
};
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
    let stats = scan_streaming(options, |app| apps.push(app), |_| {})?;
    sort_apps(&mut apps, options.sort, options.sort_direction);
    Ok(ScanOutcome { apps, stats })
}

/// 把结果排成确定性顺序：主键由 `key` 定、方向由 `direction` 定，
/// **次级键恒为路径升序**（这样同主键下的次序永远确定）。
///
/// 方向必须在比较器里表达，不能事后 `reverse()` —— 那会把次级键一起翻转，
/// 变成「size 升序 + 路径降序」。
pub fn sort_apps(apps: &mut [AppInfo], key: SortKey, direction: Direction) {
    apps.sort_by(|left, right| {
        let primary = match key {
            SortKey::Size => left.size.cmp(&right.size),
            SortKey::Kind => left.kind.rank().cmp(&right.kind.rank()),
            SortKey::Path => left.path.cmp(&right.path),
        };
        let primary = match direction {
            Direction::Desc => primary.reverse(),
            Direction::Asc => primary,
        };
        primary.then_with(|| left.path.cmp(&right.path))
    });
}

/// 流式扫描：每识别出一个应用就回调一次，**边算边回调**。
///
/// `on_notice` 在**后端刚选定的那一刻**被调用一次，早于任何 `on_app`。
///
/// # Errors
///
/// 同 `scan()`。
pub fn scan_streaming<F, G>(
    options: &ScanOptions,
    on_app: F,
    on_notice: G,
) -> Result<ScanStats, ScanError>
where
    F: FnMut(AppInfo) + Send,
    G: FnOnce(ScanNotice),
{
    let started = Instant::now();

    // 先挑后端再枚举进程。
    let (candidates, backend_name, dirs_scanned) = discover(options)?;
    on_notice(ScanNotice {
        backend: backend_name,
    });

    let running: HashSet<ProcessKey> = if options.detect_running {
        process::running_processes()
    } else {
        HashSet::new()
    };

    let threads = resolve_scan_threads(options.scan_threads);

    let detected = group(&candidates, threads);
    let roots: Vec<PathBuf> = detected.iter().map(|app| app.root.clone()).collect();

    // 计量并行执行，回调需加锁。
    //
    // 统计只要体积，所以按 `index` 落位收集 `sizes`，不再为每条结果 clone 一个
    // `AppInfo`（两个 PathBuf 的堆分配）。`detected` 与结果一一对应，本来就是现成的。
    let on_app = Mutex::new(on_app);
    let sizes: Mutex<Vec<u64>> = Mutex::new(vec![0_u64; detected.len()]);

    sizes_parallel_each(&roots, threads, |index, _path, size| {
        let app = &detected[index];
        let is_running = app
            .executable
            .as_deref()
            .is_some_and(|path| process::is_running(&running, path));

        (on_app
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))(to_app_info(
            app, is_running, size,
        ));
        sizes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[index] = size;
    });

    let sizes = sizes
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let stats = ScanStats {
        backend: backend_name,
        dirs_scanned,
        candidates: candidates.len(),
        apps: detected.len(),
        sum_bytes: sizes.iter().sum(),
        total_bytes: deduplicated_total(&detected, &sizes),
        // `as_millis()` 是 u128：`as u64` 会静默截断（虽然一次扫描不可能跑 5.8 亿年），
        // 用 `try_from` + 饱和把「不可能发生」写出来。
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
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
///
/// `sizes[i]` 必须与 `detected[i]` 对应（调用方保证两者等长且同序）。
fn deduplicated_total(detected: &[DetectedApp], sizes: &[u64]) -> u64 {
    let mut total = 0_u64;
    for (index, app) in detected.iter().enumerate() {
        let nested = detected.iter().enumerate().any(|(other_index, other)| {
            other_index != index && crate::filter::path_starts_with(&app.root, &other.root)
        });
        if nested {
            continue;
        }
        total = total.saturating_add(sizes[index]);
    }
    total
}

/// 只回答"这次扫描会选哪个后端"，**不扫描、不查询索引服务**。
#[must_use]
pub fn detect_backend(options: &ScanOptions) -> &'static str {
    match options.backend {
        Backend::Filesystem => FILESYSTEM_BACKEND,
        Backend::Auto | Backend::Index => index_service_name().unwrap_or(FILESYSTEM_BACKEND),
    }
}

/// 索引服务的展示名；服务不在场时返回 `None`。
#[cfg(all(feature = "everything", target_os = "windows"))]
fn index_service_name() -> Option<&'static str> {
    everything::is_service_available().then_some(everything::SERVICE_NAME)
}

#[cfg(not(all(feature = "everything", target_os = "windows")))]
fn index_service_name() -> Option<&'static str> {
    None
}

/// 取得候选文件。索引后端失败时按策略回落。
fn discover(options: &ScanOptions) -> Result<(Vec<Candidate>, &'static str, u64), ScanError> {
    match options.backend {
        Backend::Filesystem => {
            let result = walk(options)?;
            Ok((result.candidates, FILESYSTEM_BACKEND, result.dirs_scanned))
        }
        Backend::Index => match try_index_candidates(options) {
            Ok((candidates, service)) => Ok((candidates, service, 0)),
            Err(error) => Err(ScanError::BothBackendsFailed {
                index: error.to_string(),
                fallback: "fallback disabled by --backend index".into(),
            }),
        },
        Backend::Auto => match try_index_candidates(options) {
            Ok((candidates, service)) => Ok((candidates, service, 0)),
            Err(_index_error) => {
                let result = walk(options)?;
                Ok((result.candidates, FILESYSTEM_BACKEND, result.dirs_scanned))
            }
        },
    }
}

/// 索引后端返回候选**和它实际用的服务名**。
#[cfg(all(feature = "everything", target_os = "windows"))]
fn try_index_candidates(options: &ScanOptions) -> io::Result<(Vec<Candidate>, &'static str)> {
    everything::query_candidates(options)
}

#[cfg(not(all(feature = "everything", target_os = "windows")))]
fn try_index_candidates(_options: &ScanOptions) -> io::Result<(Vec<Candidate>, &'static str)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the index backend requires Windows and the `everything` feature",
    ))
}

fn resolve_scan_threads(configured: usize) -> usize {
    if configured > 0 {
        return configured;
    }
    std::thread::available_parallelism().map_or(2, std::num::NonZero::get)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AppKind;
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

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
            "cefscan-scan-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    /// 造一棵最小但**能被认出来**的应用树。
    fn write_app(root: &Path, name: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(if cfg!(windows) {
                "libcef.dll"
            } else {
                "libcef.so"
            }),
            b"not a real binary",
        )
        .unwrap();
        fs::write(
            dir.join(if cfg!(windows) {
                "chrome.exe"
            } else {
                "chrome"
            }),
            b"x",
        )
        .unwrap();
    }

    fn options_for(root: &Path, backend: Backend) -> ScanOptions {
        ScanOptions {
            roots: vec![root.to_path_buf()],
            backend,
            ..ScanOptions::default()
        }
    }

    fn app(path: &str, kind: AppKind, size: u64) -> AppInfo {
        AppInfo {
            path: PathBuf::from(path),
            root: PathBuf::from(path),
            kind,
            size,
            running: false,
            evidence: None,
        }
    }

    /// 默认口径：占用降序，同尺寸时路径升序。
    #[test]
    fn size_sort_is_descending_with_ascending_paths() {
        let mut apps = vec![
            app(r"D:\b", AppKind::Cef, 10),
            app(r"D:\c", AppKind::Cef, 20),
            app(r"D:\a", AppKind::Cef, 20),
        ];

        sort_apps(&mut apps, SortKey::Size, Direction::Desc);

        let sizes: Vec<u64> = apps.iter().map(|app| app.size).collect();
        assert_eq!(sizes, vec![20, 20, 10], "占用必须降序");
        assert_eq!(apps[0].path, PathBuf::from(r"D:\a"), "同尺寸时路径升序");
        assert_eq!(apps[1].path, PathBuf::from(r"D:\c"));
    }

    /// `--sort kind` 必须与 `--sort path` 排出**不同**顺序。
    ///
    /// 曾经 `SortArg::Kind` 被压成一个 bool 落进 else 分支，`kind` 静默退化成 `path`。
    #[test]
    fn sort_kind_differs_from_sort_path() {
        let base = [
            app(r"D:\a-chrome\chrome.exe", AppKind::Chrome, 10),
            app(r"D:\z-electron\app.exe", AppKind::Electron, 20),
        ];

        let mut by_path = base.to_vec();
        sort_apps(&mut by_path, SortKey::Path, Direction::Asc);
        assert_eq!(by_path[0].kind, AppKind::Chrome, "路径升序时 a-chrome 在前");

        let mut by_kind = base.to_vec();
        sort_apps(&mut by_kind, SortKey::Kind, Direction::Desc);
        assert_eq!(
            by_kind[0].kind,
            AppKind::Electron,
            "类型降序时 Electron（rank 100）在前"
        );

        assert_ne!(
            by_path
                .iter()
                .map(|app| app.path.clone())
                .collect::<Vec<_>>(),
            by_kind
                .iter()
                .map(|app| app.path.clone())
                .collect::<Vec<_>>(),
            "kind 与 path 必须排出不同顺序"
        );
    }

    /// 升序只翻转主键，**次级键仍是路径升序**。
    ///
    /// 曾经用 `apps.reverse()` 实现 `--ascending`，把路径也一起翻成降序。
    #[test]
    fn ascending_flips_only_the_primary_key() {
        let mut apps = vec![
            app(r"D:\b", AppKind::Cef, 20),
            app(r"D:\a", AppKind::Cef, 20),
        ];

        sort_apps(&mut apps, SortKey::Size, Direction::Asc);

        assert_eq!(apps[0].path, PathBuf::from(r"D:\a"), "同尺寸时路径仍升序");
        assert_eq!(apps[1].path, PathBuf::from(r"D:\b"));
    }

    /// 后端名在第一条结果之前送达。
    #[test]
    fn notice_reports_the_backend_before_any_result() {
        let fixture = fixture("notice");
        write_app(&fixture.root, "app");

        let log = Mutex::new(Vec::new());
        let stats = scan_streaming(
            &options_for(&fixture.root, Backend::Filesystem),
            |app| {
                log.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("app:{}", app.path.display()));
            },
            |notice| {
                log.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("notice:{}", notice.backend));
            },
        )
        .unwrap();

        let log = log
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(log.first().map(String::as_str), Some("notice:cefscan"));
        assert_eq!(log.len(), 2, "通知只发一次，然后才是结果：{log:?}");
        assert_eq!(stats.backend, "cefscan");
        assert_eq!(stats.apps, 1);
    }

    /// 没有索引服务可用时，"自动"必须悄悄回落到遍历，并且**如实**说自己是谁。
    #[cfg(not(all(feature = "everything", target_os = "windows")))]
    #[test]
    fn auto_backend_falls_back_to_cefscan_and_says_so() {
        let fixture = fixture("auto-fallback");
        write_app(&fixture.root, "app");

        let seen = Mutex::new(None);
        let stats = scan_streaming(
            &options_for(&fixture.root, Backend::Auto),
            |_| {},
            |notice| *seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(notice.backend),
        )
        .unwrap();

        let seen = seen.into_inner().unwrap_or_else(|e| e.into_inner());
        assert_eq!(seen, Some("cefscan"));
        assert_eq!(stats.backend, "cefscan");
        assert_eq!(stats.apps, 1);
    }

    /// 显式要求索引后端时**不许**回落，直接报错。
    #[cfg(not(all(feature = "everything", target_os = "windows")))]
    #[test]
    fn index_backend_without_a_service_is_an_error() {
        let fixture = fixture("index-unsupported");
        write_app(&fixture.root, "app");

        let error = scan(&options_for(&fixture.root, Backend::Index)).unwrap_err();
        assert!(
            matches!(error, ScanError::BothBackendsFailed { .. }),
            "期望 BothBackendsFailed，实得 {error}"
        );
    }

    /// 探测（`detect_backend`）和真扫描（`ScanStats::backend`）报同一个名字。
    #[test]
    fn probe_and_scan_report_the_same_backend() {
        let fixture = fixture("probe-agrees");
        write_app(&fixture.root, "app");

        let options = options_for(&fixture.root, Backend::Filesystem);
        let outcome = scan(&options).unwrap();

        assert_eq!(detect_backend(&options), FILESYSTEM_BACKEND);
        assert_eq!(outcome.stats.backend, detect_backend(&options));
    }

    /// 没有索引服务可用时，"自动"的探测结果必须是遍历后端。
    #[cfg(not(all(feature = "everything", target_os = "windows")))]
    #[test]
    fn probe_reports_cefscan_for_auto_without_a_service() {
        assert_eq!(
            detect_backend(&ScanOptions {
                backend: Backend::Auto,
                ..ScanOptions::default()
            }),
            FILESYSTEM_BACKEND
        );
    }

    /// 探测**不能**等索引查询。
    #[test]
    fn probe_never_waits_on_the_index_timeout() {
        let options = ScanOptions {
            backend: Backend::Auto,
            index_timeout: Duration::from_secs(30),
            ..ScanOptions::default()
        };

        let started = Instant::now();
        let name = detect_backend(&options);
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "探测花了 {elapsed:?}，说明它在等一次真实的索引查询"
        );
        assert!(!name.is_empty(), "后端名不能是空串");
    }
}
