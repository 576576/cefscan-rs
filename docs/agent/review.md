# 代码评审：函数式构造与最佳实践

> 评审快照，2026-10-06（版本 `0.0.64`），覆盖 `crates/` 全部 21 个 `.rs`（4540 行）。
> 条目处理完就删掉对应段落，整份文件清空后可以删除。规则编号对应 `rust-skills`。

## 结论

**风格定位是对的**：纯函数负责「判定与变换」（`filter` / `naming` / `candidate` / `signature`
全是 `&T -> bool` / `Option<T>`，不碰 I/O，好测），命令式循环留在「热路径与并发编排」
（`walk` / `size` / `inspect` / `scan`）。这是 Rust 的惯用取舍，不必追求纯 FP。

真正的缺口集中在三类：

1. **该用累加器/组合子的地方手写了可变状态机**（`inspect_directory`、`insert`、
   `strongest_in_chunk`、`allows_path`）。
2. **「形式上是链式、实质还是命令式」**：`map(..).unwrap_or(..)`、`if let/else` 不用
   `map_or_else`、`matches!` 不合并 or-pattern —— clippy 已报 20+ 处。
3. **1 个真 bug + 4 处热路径上的重复分配**。

---

## 一、正确性问题

### P0-1 `--sort kind` 静默失效（cli.rs:117 + scan.rs:42）

```rust
// cli.rs
sort_by_size: matches!(self.sort, SortArg::Size),   // SortArg::Kind 落到 else 分支
```
`SortArg` 有三个取值，但 core 只认一个 bool。`--sort kind` 与 `--sort path` **输出完全一致**。
实测（`dist/sortfix` 夹具，chrome 在前 / cef 在后）：

```
=== --sort path ===        === --sort kind ===        === --sort size ===
chrome ...                 chrome ...                 cef ...
cef    ...                 cef    ...                 chrome ...
```

`--help` 里 `kind` 是合法取值，属于「文档承诺了但没实现」。这是本次评审唯一的真 bug。

**方案**：把 bool 换成枚举，让类型系统兜住。

```rust
// model.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey { #[default] Size, Path, Kind }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction { #[default] Desc, Asc }

// scan.rs
pub fn sort_apps(apps: &mut [AppInfo], key: SortKey, dir: Direction)
```
顺带解决 `sort_apps(apps, by_size: bool)` 的布尔参数问题（`api-parse-dont-validate`、
`type-no-stringly`）。注意 `SortKey::Kind` 的语义要定死：建议按 `AppKind::rank()` 降序、
同 rank 再按路径升序（与 `insert` 的「取最强」口径一致）。

### P0-2 `--ascending` 用 `reverse()` 反转了次级键（main.rs:49-51）

```rust
if args.ascending { apps.reverse(); }
```
`sort_apps` 是「size 降序 + path 升序」，`reverse()` 之后变成「size 升序 + **path 降序**」——
和文档写的「否则按路径升序」不一致。方向应该在比较器里表达，不是事后翻转。

### P1-1 `read_utf16_z` 的 `Option` 是死的（everything.rs:201-224）

签名 `io::Result<Option<Vec<u16>>>`，但函数体**只会**返回 `Ok(Some(_))` 或 `Err`：
offset 非法走 `Err`，读到 NUL 走 `Ok(Some(units))`。调用点（185-188）的
`.as_deref().map(..)` 因此永远命中 `Some`。

`type-option-nullable`：`Option` 必须承载「可能不存在」的语义，否则是骗人的类型。
**方案**：改成 `io::Result<Vec<u16>>`，调用点直接 `OsString::from_wide(&units)`。

### P1-2 `parse_size` 的重复 match 臂（cli.rs:170-172）

```rust
"" => 1.0,
"b" => 1.0,   // clippy: these match arms have identical bodies
```
合并成 `"" | "b" => 1.0,`（`pat-at-bindings` / or-pattern）。

### P1-3 `RegisterClassExW` 的空 if 块（everything.rs:262-264）

```rust
if RegisterClassExW(&info) == 0 {
    // 已存在同名类时也会失败，忽略即可。
}
```
`anti-empty-catch`。写成 `let _ = unsafe { RegisterClassExW(&info) };` 加同样的注释。

---

## 二、性能：热路径上的重复分配

按预估收益排序。前四条都在「每个文件 / 每个进程 / 每个目录」的量级上。

### PERF-1 每扫一个文件分配并清零 1 MiB 缓冲（signature.rs:69）

```rust
let mut buffer = vec![0_u8; CHUNK_SIZE + OVERLAP];   // 1,048,640 字节，每次调用
```
`scan_read` 每处理一个候选文件就来一次 `alloc + memset(1 MiB)`，约 100 µs。全盘扫描
几千个可执行文件 = **秒级**纯 memset。

`group.rs:189` 已经是 `.map_init(SignatureScanner::new, ..)`，每个 rayon 任务独占一个
scanner —— 正好是放复用缓冲的地方。

**方案**：`scan_read(&mut self, ..)`，`SignatureScanner` 加一个 `buffer: Vec<u8>` 字段，
首次分配后一直复用（`mem-reuse-collections`）。`scan_file` / `inspect_directory` 跟着改成
`&mut SignatureScanner`。

### PERF-2 每个进程分配 64 KiB 缓冲（process.rs:62）

```rust
loop {
    ...
    let mut buffer = vec![0_u16; 32_768];   // 在循环体内
```
约 300 个进程 → 19 MB 的 alloc + memset。**挪到循环外**，每轮只重置 `length`。

### PERF-3 `sort_by_key` 的 key 函数被调用 O(n log n) 次（inspect.rs:23）

```rust
entries.sort_by_key(|entry| entry.path());   // entry.path() 每次都分配一个 PathBuf
```
`sort_by_key` 的实现是 `sort_by(|a, b| f(a).cmp(&f(b)))`，所以 key 函数每次比较都调两次。
实测（`n = 64`）：

```
sort_by_key         调用 key 函数 126 次
sort_by_cached_key  调用 key 函数 64 次
```

在目录条目数上百的目录里就是几百次 PathBuf 分配。**方案**：`sort_by_cached_key`
（`perf-collect-once` 的同类问题），或 `sort_by_cached_key(|e| e.file_name())`。

### PERF-4 每条结果 clone 一次 `AppInfo`（scan.rs:102）

```rust
(on_app.lock()...)(info.clone());          // 2 个 PathBuf 堆分配
collected.lock()...push(info);
```
`collected` 只用来算 `sum_bytes` / `total_bytes` / `apps.len()`，却为此把整条结果复制一遍。

**方案**：统计只需要 `size`，而 `detected`（`group` 的产物）已经持有 root 且与结果一一对应。
收集 `sizes: Vec<u64>`（按 `index` 落位）即可，**零 AppInfo 克隆**：

```rust
let sizes = Mutex::new(vec![0_u64; detected.len()]);
sizes_parallel_each(&roots, threads, |index, _path, size| {
    let app = &detected[index];
    let is_running = app.executable.as_deref().is_some_and(|p| process::is_running(&running, p));
    (on_app.lock()...)(to_app_info(app, is_running, size));
    sizes.lock()...[index] = size;
});
```
`sum_bytes` = `sizes.iter().sum()`，`deduplicated_total` 改成吃 `(detected, &sizes)`。

### PERF-5 遍历结束时整体 clone 候选向量（walk.rs:76-77）

```rust
let mut candidates = results.lock().unwrap_or_else(|e| e.into_inner()).clone();
```
此时 `rayon::scope` 已结束、所有 `Arc` 克隆都已 drop，`strong_count == 1`。

```rust
let mut candidates = match Arc::try_unwrap(results) {
    Ok(mutex) => mutex.into_inner().unwrap_or_else(|e| e.into_inner()),
    Err(shared) => shared.lock().unwrap_or_else(|e| e.into_inner()).clone(),
};
```
省掉「候选数 × 1 次 PathBuf 分配」。

### PERF-6 等待用 1 ms 轮询（walk.rs:134-138）

```rust
guard = shared.cvar.wait_timeout(guard, Duration::from_millis(1)).unwrap_or_else(..).0;
```
队列空、还有目录在途时，每个等待线程每秒醒 1000 次（8 线程 = 8000 次/秒）。

**为什么可以去掉超时**：所有状态变更都在同一把 mutex 下，`notify_all` 覆盖了全部会让
等待者前进的变更点（push 子目录、`pending` 归零），等待者被唤醒后也一定重新持锁复查 ——
不存在丢失唤醒的窗口。改成无条件 `wait(guard)` 即可。

> 保留超时的唯一理由是「万一有漏 notify 就死锁」。如果要保留这份保险，把 1 ms 放到
> 50~100 ms：抖动成本降两个数量级，同时仍能在漏唤醒时脱困。

### PERF-7 待观察（不建议现在动）

- `deduplicated_total`（scan.rs:137-148）与 `drop_apps_nested_in_identified_roots`
  （group.rs:154-173）都是 O(n²) 的嵌套包含判断。n 是**应用条数**（几十到几百），
  不是候选数，实际开销可忽略。真到几千条再改成「按路径长度升序 + 只与已接受的顶层根比」。
- `filter::allows_path` 对每个目录做 O(depth × (roots + excludes)) 的组件遍历。
  这是真热路径，但当前基线（`docs/agent/performance.md`）已经达标，属于「先测量再动」。

---

## 三、函数式构造：可以改得更声明式的四处

### FP-1 `inspect_directory` 的 5 个可变局部 + 提前 return（inspect.rs:30-102）

`best` / `best_path` / `best_launchable` / `fallback` 四个累加器 + 一个 `return`（Edge/Chrome
的文件名命中）。这是全仓最命令式的一段。

**方案**：抽一个累加器结构 + `ControlFlow`（`perf-iter-over-index`、`pat-let-else`）：

```rust
struct Best { kind: Option<AppKind>, path: Option<PathBuf>, launchable: bool, fallback: Option<(u8, PathBuf)> }

let best = entries.into_iter().try_fold(Best::default(), |acc, entry| {
    match classify_entry(&entry, dir, flavor, scanner) {
        Finding::Skip => ControlFlow::Continue(acc),
        Finding::Merge(finding) => ControlFlow::Continue(acc.merge(finding)),
        Finding::Decided(inspection) => ControlFlow::Break(inspection),   // 文件名命中
    }
});
```
把「单个条目怎么判」抽成纯函数 `classify_entry`，可单测、无隐藏状态。

### FP-2 手写的「取最强」循环（group.rs:140-151、signature.rs:127-139）

```rust
// signature.rs
for rule in rules {
    if rule.finder.find(chunk).is_some() {
        let candidate = (rule.kind, rule.needle);
        best = match best {
            Some(current) if current.0.rank() >= candidate.0.rank() => Some(current),
            _ => Some(candidate),
        };
    }
}
```
**注意**：直接换 `max_by_key` 会改行为 —— `max_by_key` 在并列时返回**最后一个**，而这里是
**先到先得**。standard 规则里 Electron 有两条（`third_party/electron_node` 在前），
所以 `evidence` 会从前者变成后者，`--verbose` 与 GUI 提示都会变。

正确写法是 `reduce`（保留先到先得）：

```rust
rules.iter()
    .filter(|rule| rule.finder.find(chunk).is_some())
    .map(|rule| (rule.kind, rule.needle))
    .reduce(|best, candidate| if candidate.0.rank() > best.0.rank() { candidate } else { best })
```

`group.rs:140 insert` 同理：抽成命名谓词 `fn beats(existing: &DetectedApp, candidate: &DetectedApp) -> bool`，
现在是 `Some(existing) if existing.kind.rank() > ... || (... && !existing.is_dir && detected.is_dir) => {}`
的双重否定守卫，可读性差（`perf-entry-api` 也可顺带减少一次查表）。

### FP-3 `allows_path` 的早返回链（filter.rs:52-79）

五个 `if !x { return false; }`。改成纯谓词的合取，每一块都能单独命名、单独测：

```rust
pub fn allows_path(&self, path: &Path) -> bool {
    self.in_roots(path)
        && !self.hits_excluded_path(path)
        && (self.include_hidden || !is_hidden(path))
        && !self.is_platform_excluded(path)
        && !path.components().any(|c| self.component_is_excluded(c.as_os_str()))
}
```
（`allows_dir` 与 `allows_path` 同体，保留别名即可。）

### FP-4 手写索引循环（filter.rs:147-163、naming.rs:114-122、everything.rs:176-191）

```rust
// filter.rs::path_starts_with —— 热路径，每目录每 root 一次
for (index, expected) in root.iter().enumerate() {
    if ascii_fold(path[index]) != ascii_fold(*expected) { return false; }
}
```
→ `path.iter().zip(root).all(|(a, b)| ascii_fold(*a) == ascii_fold(*b))`
（`anti-index-over-iter`，顺带消掉逐次边界检查，`opt-bounds-check`）。

```rust
// naming.rs::strip_package_suffix
name.as_bytes().windows(2)
    .position(|w| w[0] == b'_' && w[1].is_ascii_digit())
    .map_or(name, |index| &name[..index])
```

```rust
// everything.rs::parse_reply
(0..item_count).map(|index| -> io::Result<ReplyItem> { ... }).collect::<io::Result<Vec<_>>>()
```
顺带删掉 `Vec::with_capacity(item_count.min(4096))` 里那个魔数 4096 —— range 的
`size_hint` 是精确的，`collect` 会自己按 `item_count` 预留。

---

## 四、clippy 已报但没人管的（`pedantic` 共 ~110 条）

CI 只跑默认 lint 组 + `-D warnings`，所以下面这些一直静默存在。挑高价值的：

| 类别 | 处数 | 代表位置 | 处理 |
| --- | --- | --- | --- |
| `redundant closure` | 18 | `.unwrap_or_else(\|e\| e.into_inner())` | 换 `PoisonError::into_inner` |
| `implicit borrow as raw pointer` | 9 | `&mut info as *mut _` | 换 2024 的 `&raw mut info`（见 §六） |
| `#[must_use]` 缺失 | 20 | `display_name` / `classify_candidate_name` / `dir_size` / `human_size` / `is_executable_magic` | 纯函数批量补 |
| `map().unwrap_or()` | 6 | scan.rs:211,238；process.rs:118；everything.rs:322 | 换 `map_or` / `is_ok_and` |
| `let ... else` | 1 | filter.rs:117 | 换 `let Some(text) = .. else { return false };` |
| `unnested or-patterns` | 1 | filter.rs:158 | `Some(b'/' \| b'\\')` |
| `unused self` | 1 | filter.rs:91（Windows 分支） | 改成自由函数或关联函数 |
| `# Errors` 文档缺失 | 3 | walk.rs:26、signature.rs:59,100 | 补 |
| `more than 3 bools` | 2 | `Cli`（6 个）、`ScanOptions`（4 个） | 见 P0-1 |

**工程化建议**：加 `[workspace.lints]`（`lint-workspace-lints`），把
`clippy::pedantic` 的高价值子集显式打开并 `-D` 掉，让 CI 拦住新增；一次性
`cargo clippy --fix` 能自动改掉 48 处（core）+ 4 处（wins）。

---

## 五、类型与 API 设计

- **`AppKind` 的 label 双份维护**：`AppKind::label()`（model.rs:39）与
  `serde(rename_all = "snake_case")`（model.rs:10）必须永远一致，但没有测试钉住。
  建议加 `AppKind::ALL: [Self; 9]` + 一个断言 `serde_json::to_value(k) == k.label()` 的测试。
- **`parse_kind` 手写变体表**（cli.rs:138-158）：遍历 9 个变体比 label，错误信息里又抄了
  一遍 label 列表。有了 `AppKind::ALL` 之后可以 `impl FromStr for AppKind`（`conv-fromstr-parsing`），
  `parse_kind` 退化成 `raw.trim().to_ascii_lowercase().parse()`，错误信息由 `ALL` 生成。
- **Backend 字符串映射有三份**：`cli::BackendArg`（clap）、Tauri 的 `to_options`
  （lib.rs:151-155）、core 的 `Backend`。建议在 core 加 `Backend::label()` /
  `Backend::from_label(&str) -> Option<Self>`，Tauri 侧直接复用；CLI 的 clap 枚举保留
  （`ValueEnum` 需要），但 `scan_options` 里的手写 match 可以删。
  > 顺带一个不一致：CLI 对未知 `--backend` **报错**，Tauri 对未知值**静默回落 Auto**
  > （lib.rs:154 的 `_ =>`）。GUI 容错是合理的，但值得写进文档。
- **`#[non_exhaustive]` 不一致**：`AppKind` / `ScanError` 有，`Backend` / `ScanOptions` /
  `AppInfo` / `ScanStats` / `Candidate` 没有。都是同一个 workspace 内部 crate，
  要么统一加（对外发布友好），要么统一不加（内部工具，改字段就一起改）。现在这样最难受。
- **`human_size` 的 `u64 as f64`**（output.rs:106）：超过 2^53 会丢精度。显示用无所谓，
  但既然是「给人看」的函数，用整数除法更稳（`opt`/`num-cast-try-from`）。
- **`elapsed_ms: ..as_millis() as u64`**（scan.rs:119）：u128 → u64 的 `as`。
  `u64::try_from(..).unwrap_or(u64::MAX)` 表达意图更清楚。

---

## 六、unsafe 与 2024 edition

**做得好的**：每一处 `unsafe` 都有 `// SAFETY:` 且写清了不变量；`OwnedHandle` RAII 管
Win32 句柄；`ReplyWindow` 的 `Arc::into_raw`/`from_raw` 配平正确（create 里 +1、Drop 里
-1 + 字段自身 -1）；`to_wide` / `to_wide_z` 严格分离并有测试钉住。

可以改的：

- **`&mut x as *mut _` → `&raw mut x`**（9 处，everything.rs / icon.rs / process.rs）。
  2024 edition 的写法：前者要先构造一个 `&mut` 引用再转指针（引用必须有效），
  后者直接取地址，没有「临时引用必须合法」这个隐含前提。这是纯粹的收紧。
- **`try_into().unwrap()`**（everything.rs:198, 217）：切片长度已由构造保证，
  换成 `.expect("4 字节切片由长度检查保证")` 把理由写出来（`err-expect-bugs-only`）。
- **`_wparam` 在函数体里被使用**（everything.rs:395）：改名 `wparam`（clippy）。
- **没跑 Miri**（`unsafe-miri-ci`）：合理 —— 这些 unsafe 全是 Win32 调用，Miri 跑不了。
  但 `is_executable_magic`、`read_u32`、`read_utf16_z` 这些**纯字节解析**是可以进 Miri 的，
  值得单开一个 `cfg(miri)` 的测试入口。
- **锁毒化的处理不一致**：大部分地方是 `unwrap_or_else(|e| e.into_inner())`（好），
  但 `scan.rs:102,103` 用 `.expect("callback poisoned")`，`icon.rs:15,21` 用
  `.lock().unwrap()`。后者在生产代码里会 panic（`err-no-unwrap-prod`）—— 图标取不到只是
  少个图标，不该崩掉整个 GUI。统一成 `unwrap_or_else(PoisonError::into_inner)`。

---

## 七、死代码与依赖

| 项 | 位置 | 说明 |
| --- | --- | --- |
| `dir_name_set` | filter.rs:186-188 | 只有定义，无调用（`pub` 所以没有 dead_code 警告） |
| `ScanResult<T>` | error.rs:34 + lib.rs:26 | 只有定义与 re-export，`scan.rs` 全写 `Result<_, ScanError>` |
| `AppKind::strongest` | model.rs:54-60 | 只有定义，无调用 |
| `sizes_parallel` | size.rs:91-99 | 只有测试用；`scan.rs` 走的是 `sizes_parallel_each` |
| `anyhow` | cefscan-cli/Cargo.toml | 全仓 0 处引用，可以从依赖里删掉 |

前四项要么删掉，要么补调用点；`ScanResult` 若想保留就统一用起来（现在是两套写法并存）。

---

## 八、建议的落地顺序

1. **P0-1 / P0-2**（`--sort` 三值 + 方向）：唯一的功能性 bug，改了顺带把 bool 参数换成枚举。
   影响 CLI 行为，需要同步 `docs/user-guide.md` 的参数表。
2. **PERF-1 / PERF-2**（1 MiB 与 64 KiB 缓冲）：改动小、收益直接、不动语义。
3. **P1-* / §四**：`cargo clippy --fix` 先吃掉 52 处机械项，剩下的人工过。
4. **PERF-3 / PERF-4 / PERF-5**：都在核心路径上，建议**先补基准再改**（`perf-profile-first`），
   用 `docs/agent/performance.md` 现有基线做前后对照。
5. **FP-1 ~ FP-4**：纯可读性重构，零行为变化，可以合并成一次提交；每步都靠现有测试兜底。
6. **§五 / §七**：类型与清理，随缘做。

改完记得：`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test`、
`node tools/ui_harness.js` 三件套，以及刷新 `dist/`。
