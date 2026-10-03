//! 扫描编排：遍历 → 归并 → 计量 → 运行态。

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use crate::error::ScanError;
use crate::group::{DetectedApp, group};
use crate::model::{
    AppInfo, Backend, Candidate, FILESYSTEM_BACKEND, ScanNotice, ScanOptions, ScanStats,
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
///
/// `on_notice` 在**后端刚选定的那一刻**被调用一次，早于任何 `on_app`。
/// 用 `FnOnce` 而不是 `FnMut`：它在语义上只该发生一次，而且 `FnOnce` 对调用方
/// 更宽松（不需要可变借用）。它也不需要 `Send`——通知在调用者线程上同步发出，
/// 不进任何工作线程池。
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

    // 先挑后端再枚举进程：后端名要第一时间报出去（GUI 靠它把"自动"变成
    // "自动（cefscan）"），而进程枚举跟选后端毫无关系，放到后面能让这条通知
    // 早几十毫秒到达。
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
            other.root != app.root && crate::filter::path_starts_with(&app.root, &other.root)
        }) {
            continue;
        }
        total = total.saturating_add(app.size);
    }
    total
}

/// 只回答"这次扫描会选哪个后端"，**不扫描、不查询索引服务**。
///
/// 和 [`scan_streaming`] 的 `on_notice` 是一回事，区别只在时机：那个要等真开扫
/// （`discover()` 挑完后端）才知道，这个在**开扫之前**就能问。GUI 靠它把工具栏上
/// 那个"自动"立刻变成一个具体的后端名——进工具模式时刷一次，用户点一下 chip
/// 再刷一次，都不该等用户点"开始扫描"。
///
/// 所以它必须便宜：索引探测只看 Everything 的窗口在不在（`FindWindowW`），
/// 不发查询、不等 `index_timeout`。代价是它只承诺"会选谁"，不保证那次查询一定
/// 成功；真开扫时后端仍可能超时并（在 `Auto` 下）回落到遍历。
///
/// 必须与 `discover()` 的选择策略保持一致，否则 chip 上显示的和结果里报的会是两个东西。
pub fn detect_backend(options: &ScanOptions) -> &'static str {
    match options.backend {
        // 遍历后端不依赖任何外部条件，永远可用。
        Backend::Filesystem => FILESYSTEM_BACKEND,
        // "自动"和"只用索引"的差别只在**失败之后**：前者回落遍历，后者报错。
        // 挑后端那一刻两者看到的可用性判断是同一个，所以名字也一样。
        Backend::Auto | Backend::Index => index_service_name().unwrap_or(FILESYSTEM_BACKEND),
    }
}

/// 索引服务的展示名；服务不在场时返回 `None`。
///
/// "有没有索引服务"的唯一判据，探测和真查询共用同一份窗口类名列表。
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

/// 索引后端返回候选**和它实际用的服务名**，服务名直接进结果，用户能看到是谁干的活。
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
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;
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
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    /// 造一棵最小但**能被认出来**的应用树。
    ///
    /// 两个文件缺一不可：`libcef.*` 让遍历阶段产出候选，`chrome` / `chrome.exe`
    /// 让 `inspect_directory` 只凭文件名就定死类型（不必真读文件内容），
    /// 所以这个 fixture 在两个平台上都成立。
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

    /// 后端名必须在**第一条结果之前**送达，否则 GUI 里的"自动（cefscan）"
    /// 就退化成"扫描完才告诉你"，等于白做。
    ///
    /// 顺带守住 filter 的一个历史 bug：fixture 建在 `temp_dir()` 下，Linux 上
    /// 就是 `/tmp`——它在 `PLATFORM_EXCLUDED_ROOTS` 里，但作为**显式 root**
    /// 必须照扫不误。所以下面那句"找到了 1 个应用"不是废话断言。
    #[test]
    fn notice_reports_the_backend_before_any_result() {
        let fixture = fixture("notice");
        write_app(&fixture.root, "app");

        let log = Mutex::new(Vec::new());
        let stats = scan_streaming(
            &options_for(&fixture.root, Backend::Filesystem),
            |app| {
                log.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("app:{}", app.path.display()));
            },
            |notice| {
                log.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("notice:{}", notice.backend));
            },
        )
        .unwrap();

        let log = log.into_inner().unwrap_or_else(|e| e.into_inner());
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

    /// 显式要求索引后端时**不许**回落：宁可报错，也不给用户一个"我按你说的做了"
    /// 的假象。
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

    /// 探测（`detect_backend`）和真扫描（`ScanStats::backend`）必须报同一个名字。
    ///
    /// 这是 GUI 那条"chip 上写着自动（cefscan），结果汇总里也写着 cefscan"的
    /// 依据。两者分头实现，一旦策略漂移，用户会看到自相矛盾的两个后端名。
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
    ///
    /// 把 `index_timeout` 设成 30 s：如果哪天有人把探测改成"真发一次查询再等回复"，
    /// 在有 Everything 的机器上这里会挂 30 s 然后失败。窗口探测（`FindWindowW`）
    /// 是毫秒级的，所以 1 s 的上限足够宽松，不会因为机器慢而误报。
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
