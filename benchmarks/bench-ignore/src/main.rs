use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;

/// 模拟 cefscan 的候选判定：文件名分类（libcef.dll / *_100_*.pak / libnode.*）
fn is_candidate(name: &str) -> bool {
    if name.contains("_100_") && name.ends_with(".pak") {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower == "libcef.dll"
        || lower == "libcef.so"
        || lower.starts_with("libcef.so.")
        || lower == "libcef.dylib"
        || lower == "libnode.dll"
        || lower == "libnode.so"
        || lower.starts_with("libnode.so.")
        || lower == "libnode.dylib"
}

fn builder(root: &Path) -> WalkBuilder {
    let mut b = WalkBuilder::new(root);
    // 与 cefscan 计划一致：关掉所有 gitignore/ignore 过滤，只做纯枚举 + filter_entry
    b.standard_filters(false)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(false)
        .git_exclude(false)
        .follow_links(false)
        .same_file_system(false);
    b
}

fn walk_parallel(root: &Path, threads: usize) -> (u64, u64, usize, Duration) {
    let files = AtomicU64::new(0);
    let dirs = AtomicU64::new(0);
    let hits = AtomicU64::new(0);
    let start = Instant::now();
    let mut b = builder(root);
    b.threads(threads);
    b.build_parallel().run(|| {
        Box::new(|result| {
            if let Ok(entry) = result {
                let ft = entry.file_type();
                if ft.is_some_and(|t| t.is_file()) {
                    files.fetch_add(1, Ordering::Relaxed);
                    let name = entry.file_name().to_string_lossy();
                    if is_candidate(&name) {
                        hits.fetch_add(1, Ordering::Relaxed);
                    }
                } else if ft.is_some_and(|t| t.is_dir()) {
                    dirs.fetch_add(1, Ordering::Relaxed);
                }
            }
            ignore::WalkState::Continue
        })
    });
    let elapsed = start.elapsed();
    (
        files.load(Ordering::Relaxed),
        dirs.load(Ordering::Relaxed),
        hits.load(Ordering::Relaxed) as usize,
        elapsed,
    )
}

fn walk_serial(root: &Path) -> (u64, u64, usize, Duration) {
    let start = Instant::now();
    let mut files = 0u64;
    let mut dirs = 0u64;
    let mut hits = 0usize;
    for result in builder(root).build() {
        if let Ok(entry) = result {
            let ft = entry.file_type();
            if ft.is_some_and(|t| t.is_file()) {
                files += 1;
                let name = entry.file_name().to_string_lossy();
                if is_candidate(&name) {
                    hits += 1;
                }
            } else if ft.is_some_and(|t| t.is_dir()) {
                dirs += 1;
            }
        }
    }
    (files, dirs, hits, start.elapsed())
}

fn bench<F: FnMut() -> Duration>(label: &str, reps: usize, mut f: F) -> Vec<Duration> {
    let mut runs = Vec::with_capacity(reps);
    for _ in 0..reps {
        runs.push(f());
    }
    let best = runs.iter().min().unwrap();
    println!(
        "  {:<22} best {:>8.1} ms   all: {}",
        label,
        best.as_secs_f64() * 1000.0,
        runs.iter()
            .map(|d| format!("{:.0}", d.as_secs_f64() * 1000.0))
            .collect::<Vec<_>>()
            .join(" / ")
    );
    runs
}

// ---------- 签名扫描（CPU 密集）微基准 ----------
const CHUNK: usize = 1024 * 1024;
const OVERLAP: usize = 64;
const NEEDLES: [&[u8]; 4] = [
    b"third_party/electron_node",
    b"register_atom_browser_web_contents",
    b"cef_string_utf8_to_utf16",
    b"CefSharp.Internals",
];

fn finders() -> Vec<memchr::memmem::Finder<'static>> {
    NEEDLES
        .iter()
        .map(|n| memchr::memmem::Finder::new(n).into_owned())
        .collect()
}

/// 复刻 cefscan 的分块扫描：1 MiB 块 + 64 B 重叠，命中即计
fn scan_buffer(buf: &[u8], finders: &[memchr::memmem::Finder<'static>]) -> usize {
    let mut hits = 0;
    let mut pos = 0;
    while pos < buf.len() {
        let end = (pos + CHUNK).min(buf.len());
        // 重叠窗口：多带 OVERLAP 字节，避免签名跨块被截断
        let win_end = (end + OVERLAP).min(buf.len());
        let window = &buf[pos..win_end];
        for f in finders {
            if f.find(window).is_some() {
                hits += 1;
                break;
            }
        }
        if end == buf.len() {
            break;
        }
        pos = end - OVERLAP; // CHUNK >> OVERLAP，必然前进
    }
    hits
}

fn sig_bench() {
    let size = 512 * 1024 * 1024; // 512 MiB
    println!("signature scan microbench (in-RAM, no disk IO)");
    println!("buffer: {} MiB, chunk {} MiB + {} B overlap, {} needles",
        size / 1024 / 1024, CHUNK / 1024 / 1024, OVERLAP, NEEDLES.len());

    let mut buf = vec![0u8; size];
    let mut state = 0x853c49e6748fea9bu64;
    for b in buf.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = (state & 0xff) as u8;
    }

    let finders = finders();
    // 单线程
    let start = Instant::now();
    let hits = scan_buffer(&buf, &finders);
    let single = start.elapsed();
    let gib = size as f64 / (1024.0 * 1024.0 * 1024.0);
    println!("  1 thread : {:>8.1} ms  {:>6.2} GiB/s  (hits={hits})",
        single.as_secs_f64() * 1000.0, gib / single.as_secs_f64());

    let parallelism = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    for threads in [2usize, 4, 8, 16] {
        if threads > parallelism {
            continue;
        }
        let start = Instant::now();
        let chunks: Vec<&[u8]> = buf.chunks(size / threads).collect();
        std::thread::scope(|s| {
            let handles: Vec<_> = chunks
                .iter()
                .map(|c| {
                    let f = &finders;
                    s.spawn(move || scan_buffer(c, f))
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
        });
        let dur = start.elapsed();
        println!(
            "  {:>2} threads: {:>8.1} ms  {:>6.2} GiB/s  {:>5.2}x",
            threads,
            dur.as_secs_f64() * 1000.0,
            gib / dur.as_secs_f64(),
            single.as_secs_f64() / dur.as_secs_f64()
        );
    }
    println!();
    println!("换算：若 30 个应用主程序合计 4 GiB，单线程约需 {:.2} s，8 线程约需 {:.2} s",
        4.0 / (gib / single.as_secs_f64()),
        4.0 / (gib / single.as_secs_f64()) / 8.0);
}

// ---------- fsindex 对照 ----------
fn fsindex_run(root: &Path, cfg: fsindex::Config) -> (u64, u64, usize, Duration) {
    let idx = fsindex::FileIndexer::with_config(root, cfg);
    let start = Instant::now();
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut hits = 0usize;
    for f in idx.files() {
        files += 1;
        bytes += f.metadata.size;
        let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if is_candidate(&name) {
            hits += 1;
        }
    }
    (files, bytes, hits, start.elapsed())
}

fn fsindex_parallel(root: &Path, cfg: fsindex::Config) -> (u64, u64, usize, Duration) {
    let idx = fsindex::FileIndexer::with_config(root, cfg);
    let start = Instant::now();
    let all = idx.files_parallel();
    let files = all.len() as u64;
    let bytes: u64 = all.iter().map(|f| f.metadata.size).sum();
    let hits = all
        .iter()
        .filter(|f| {
            f.path
                .file_name()
                .map(|n| is_candidate(&n.to_string_lossy()))
                .unwrap_or(false)
        })
        .count();
    (files, bytes, hits, start.elapsed())
}

/// 只跑遍历相关的三项，跳过会读文件内容的 fsindex 默认配置
fn compare_walk_only(root: &Path, reps: usize) {
    println!("=== 遍历对照（跳过读内容）：{} ===", root.display());
    for (label, threads) in [("ignore 8T", 8usize), ("ignore 1T", 1usize)] {
        let mut best = Duration::from_secs(u64::MAX);
        let mut f = 0;
        for _ in 0..reps {
            let (files, _d, _h, t) = walk_parallel(root, threads);
            f = files;
            best = best.min(t);
        }
        println!("  {:<12} files={f:<8} {:>9.1} ms", label, best.as_secs_f64() * 1000.0);
    }

    // 隔离"每文件额外一次 fs::metadata"的成本
    let stat_best = {
        let mut best = Duration::from_secs(u64::MAX);
        for _ in 0..reps {
            let start = Instant::now();
            let mut b = builder(root);
            b.threads(8);
            let n = std::sync::atomic::AtomicU64::new(0);
            b.build_parallel().run(|| {
                Box::new(|result| {
                    if let Ok(e) = result {
                        if e.file_type().is_some_and(|t| t.is_file()) {
                            let _ = std::fs::metadata(e.path());
                            n.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    ignore::WalkState::Continue
                })
            });
            best = best.min(start.elapsed());
        }
        best
    };
    println!(
        "  {:<12} {:>17.1} ms   (8T + 每文件 fs::metadata)",
        "ignore 8T+stat",
        stat_best.as_secs_f64() * 1000.0
    );

    let cfg_min = fsindex::Config::builder()
        .read_contents(false)
        .respect_gitignore(false)
        .include_hidden(true)
        .build();
    let mut best_c = Duration::from_secs(u64::MAX);
    let mut fc = 0;
    for _ in 0..reps {
        let (f, _b, _h, t) = fsindex_run(root, cfg_min.clone());
        fc = f;
        best_c = best_c.min(t);
    }
    println!(
        "  {:<12} files={fc:<8} {:>9.1} ms",
        "fsindex 最小",
        best_c.as_secs_f64() * 1000.0
    );
}

fn compare(root: &Path, reps: usize) {
    println!("=== 对照：{} ===", root.display());
    println!("(A) ignore WalkParallel 8 线程");
    let mut best_a = Duration::from_secs(u64::MAX);
    let mut fa = 0;
    for _ in 0..reps {
        let (f, _d, _h, t) = walk_parallel(root, 8);
        fa = f;
        best_a = best_a.min(t);
    }
    println!("    files={fa:<8} {:>8.1} ms", best_a.as_secs_f64() * 1000.0);

    println!("(B) ignore WalkParallel 1 线程（串行基线）");
    let mut best_b = Duration::from_secs(u64::MAX);
    for _ in 0..reps {
        let (_f, _d, _h, t) = walk_parallel(root, 1);
        best_b = best_b.min(t);
    }
    println!("    {:>21.1} ms", best_b.as_secs_f64() * 1000.0);

    println!("(C) fsindex 最小配置（read_contents=false, 无 gitignore, 含隐藏）");
    let cfg_min = fsindex::Config::builder()
        .read_contents(false)
        .respect_gitignore(false)
        .include_hidden(true)
        .build();
    let mut best_c = Duration::from_secs(u64::MAX);
    let mut fc = 0;
    for _ in 0..reps {
        let (f, _b, _h, t) = fsindex_run(root, cfg_min.clone());
        fc = f;
        best_c = best_c.min(t);
    }
    println!(
        "    files={fc:<8} {:>8.1} ms   -> 比 (A) 慢 {:.2}x",
        best_c.as_secs_f64() * 1000.0,
        best_c.as_secs_f64() / best_a.as_secs_f64()
    );

    println!("(D) fsindex 默认配置（read_contents=true + XXH3 哈希, 尊重 gitignore）");
    let cfg_def = fsindex::Config::default();
    let (fd, bd, hd, td) = fsindex_run(root, cfg_def.clone());
    println!(
        "    files={fd:<8} {:>8.1} ms   读取 {:.2} GiB   candidates={hd}   -> 比 (A) 慢 {:.2}x",
        td.as_secs_f64() * 1000.0,
        bd as f64 / (1024.0 * 1024.0 * 1024.0),
        td.as_secs_f64() / best_a.as_secs_f64()
    );

    println!("(E) fsindex files_parallel() 默认配置（遍历仍串行，仅后处理并行）");
    let (fe, _be, _he, te) = fsindex_parallel(root, cfg_def);
    println!(
        "    files={fe:<8} {:>8.1} ms   -> 比 (A) 慢 {:.2}x",
        te.as_secs_f64() * 1000.0,
        te.as_secs_f64() / best_a.as_secs_f64()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("sig") {
        sig_bench();
        return;
    }
    if args.first().map(|s| s.as_str()) == Some("cmpw") {
        let root = Path::new(args.get(1).map(|s| s.as_str()).unwrap_or("."));
        let reps: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
        compare_walk_only(root, reps);
        return;
    }
    if args.first().map(|s| s.as_str()) == Some("cmp") {
        let root = Path::new(args.get(1).map(|s| s.as_str()).unwrap_or("."));
        let reps: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);
        compare(root, reps);
        return;
    }
    let root = args
        .first()
        .cloned()
        .unwrap_or_else(|| ".".to_string());
    let reps: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let root = Path::new(&root);
    let parallelism = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    println!("target: {}", root.display());
    println!("logical cpus: {parallelism}   reps: {reps}");
    println!();

    println!("[single-threaded Walk::build()]");
    let (f, d, h, t) = walk_serial(root);
    println!("  -> files={f} dirs={d} candidates={h} first-run={:.1} ms (may be cold)", t.as_secs_f64()*1000.0);
    let serial = bench("serial (std::fs)", reps, || walk_serial(root).3);
    let serial_best = *serial.iter().min().unwrap();
    println!();

    println!("[ignore WalkParallel]");
    let mut results: Vec<(usize, Duration)> = Vec::new();
    for threads in [1usize, 2, 4, 6, 8, 12, 16, 24] {
        if threads > parallelism * 2 {
            continue;
        }
        let label = if threads == 1 { "threads=1".to_string() } else { format!("threads={threads}") };
        let runs = bench(&label, reps, || walk_parallel(root, threads).3);
        let best = *runs.iter().min().unwrap();
        results.push((threads, best));
    }
    println!();

    let (f, d, h, _) = walk_parallel(root, 4);
    println!("tree: files={f} dirs={d} candidates={h}");
    println!();
    println!("speedup vs threads=1:");
    let base = results[0].1.as_secs_f64();
    for (threads, dur) in &results {
        println!(
            "  {:>2} thread(s): {:>8.1} ms   {:>5.2}x   throughput {:>8.0} k entries/s",
            threads,
            dur.as_secs_f64() * 1000.0,
            base / dur.as_secs_f64(),
            (f as f64) / dur.as_secs_f64() / 1000.0
        );
    }
    println!();
    println!(
        "speedup vs serial: {:.2}x (using {} threads)",
        serial_best.as_secs_f64() / results.iter().map(|(_, d)| *d).min().unwrap().as_secs_f64(),
        results.iter().min_by_key(|(_, d)| *d).unwrap().0
    );
}
