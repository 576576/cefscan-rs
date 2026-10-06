# 性能

> 性能是本项目两条差异化主线之一（另一条是工程可测试性，见 [`testing.md`](testing.md)）。
> 各次优化的**前后对照数字**在 [`benchmark.md`](benchmark.md)；本文是措施清单与选型基线。

## 1. 措施清单

| 措施 | 说明 |
| --- | --- |
| ① 并行遍历 | 自写 `read_dir` + rayon：共享工作队列（`Mutex` + `Condvar` + `pending`），按目录粒度并行。 |
| ② 并行签名扫描 | 参考实现是逐文件串行的，本项目改为 rayon `par_iter` 扫描候选文件。 |
| ③ 预构建 `memmem::Finder` | 每个签名的 `Finder` 构建一次复用，而非每次 `memchr::memmem::find` 重建。 |
| ④ 消除热路径分配 | 候选阶段全程持有 `PathBuf` / `OsString`，只在最终输出时转 `String`；文件名匹配用 `OsStr` 字节比较 + ASCII 小写归一化，不建临时 `String`。 |
| ⑤ 目录级短路 | Edge / Chrome 靠文件名直接判定，完全不读文件内容；`unins*` / `setup*` / `report*` / `chrome-sandbox` / `crashpad_handler` 直接跳过。 |
| ⑥ 提前剪枝 | 在目录层就砍掉 `node_modules`、`WinSxS`、`$Recycle.Bin`，比事后过滤省掉整个子树遍历。 |
| ⑦ 线程本地缓冲 | 遍历线程各自持有 `Vec<Candidate>`，结束再合并，避免全量共享 `Arc<Mutex<Vec>>` 的锁竞争。 |
| ⑧ 减少 stat | 复用 `DirEntry::file_type()` 已有的元数据，不额外 `fs::metadata`。 |
| ⑨ 扫描缓冲复用 | `SignatureScanner` 自带 1 MiB 读缓冲，随 scanner 复用（`map_init` 每个 rayon 任务一份），不再是「文件数 × 1 MiB」的 alloc + memset。 |
| ⑩ 剪枝规则单遍遍历 | `Filter::allows_path` 把隐藏 / `--exclude-dir` / 回收站 / Windows 平台目录合成**一遍** `components()`，且平台目录名用 `[u8]::eq_ignore_ascii_case` 比字节，不再对每个组件 `to_ascii_lowercase()` 分配 `String`。 |
| ⑪ 去重与嵌套消解按长度剪枝 | `deduplicated_total` 与 `drop_apps_nested_in_identified_roots` 不再做 O(n²) 全表比较：按 root 长度升序处理，只跟**严格更短**的根比（长度相同不可能互相包含）。 |
| ⑫ 条件变量不轮询 | 遍历队列空了就无条件 `wait`，不再用 1 ms 超时轮询（原来队列空但还有目录在途时，8 个线程每秒白醒 8000 次）。 |


## 2. 实测基线

本机 16 逻辑核 / NTFS。下表是选型阶段用对比程序实测的（3 次取最优，单位 ms）。该对比程序
（一个独立的 `bench-ignore` crate）在选型结束后已删除，数字保留作为基线存档。

**A. 热缓存小树** `D:\Documents\GitHub`（13.1 万文件 + 1.3 万目录）

| 线程 | 1 | 2 | 4 | 6 | 8 | 12 | 16 | 24 |
|---|---|---|---|---|---|---|---|---|
| 耗时 | 814 | 441 | 267 | 211 | **198** | 206 | 218 | 237 |
| 加速 | 1.00x | 1.85x | 3.05x | 3.85x | **4.11x** | 3.96x | 3.74x | 3.44x |

**B. 冷缓存大树** `C:\Users\16695`（67 万文件 + 11 万目录，首次触碰）

| 线程 | 1 | 2 | 4 | 6 | 8 | 12 | 16 | 24 |
|---|---|---|---|---|---|---|---|---|
| 耗时 | 11418 | 6377 | 3700 | 2927 | 2662 | 2311 | **2150** | 2114 |
| 加速 | 1.00x | 1.79x | 3.09x | 3.90x | 4.29x | 4.94x | **5.31x** | 5.40x |

对照：单线程 `Walk::build()`（非并行迭代器）热缓存 12.26 s，最快并行配置 **5.80x**。

**C. 签名扫描（纯内存，排除磁盘）**：512 MiB 缓冲，1 MiB 块 + 64 B 重叠，4 条签名

| 线程 | 1 | 2 | 4 | 8 | 16 |
|---|---|---|---|---|---|
| 吞吐 | 9.92 GiB/s | 17.9 | 29.1 | **37.8** | 33.9 |
| 加速 | 1.00x | 1.80x | 2.93x | **3.81x** | 3.42x |

**结论（直接决定配置默认值）**：

1. **并行遍历的天花板是 4–5x，不是线性**。冷缓存（IO 延迟可被重叠）能吃到 16 线程的
   红利；热缓存下 8 线程就见顶，12 线程以上反而变慢。→ 遍历线程默认取
   `min(cpu, 8)`（`walk::DEFAULT_MAX_THREADS`），并暴露 `--threads` 让用户按机器调。
2. **遍历吞吐**：热缓存 16 万 → 65.9 万条目/秒；冷缓存 5.9 万 → 31.7 万条目/秒。整盘
   300 万条目的冷扫描，单线程约 50 s、8–12 线程约 10 s 量级。
3. **签名扫描是内存带宽瓶颈，不是 CPU**：单线程已 9.9 GiB/s，8 线程 3.81x 后 16 线程
   回落（超线程 + 带宽饱和）。真实场景里它更受**磁盘读取**限制。
4. **最重要的判断**：把遍历从 1x 优化到 4x，仍远不如**根本不遍历**。Everything 是索引
   查询（毫秒级），遍历是穷举（秒到十秒级）—— 两者差 1~2 个数量级。所以优先级恒为：
   **Everything 后端 > 遍历调参 > 签名并行**。

## 3. `fsindex` 评估（结论：不适合 cefscan，已实测否决）

曾考虑用 `fsindex` 0.3.1 替代 `ignore`，实测后否决。先说它是什么：

**它是建在 `ignore` 之上的"代码索引库"，不是遍历替代品。** `Cargo.toml` 显式依赖
`ignore = "0.4"`，所以"不用 ignore"实际上做不到，只是把它降为传递依赖，同时额外拉进
`notify 8` / `rayon` / `serde_json` / `thiserror 2` / `xxhash-rust`。

**遍历部分的关键实现**（`src/indexer.rs`）：

- `build_walker()` 返回的是 `builder.build()`（:460）—— **单线程 `Walk`**，没有
  `build_parallel()`。
- `files_parallel()` 源码注释原文："*Collect paths first (can't parallelize the walk
  itself easily)*"（:257）—— rayon 只用于**读内容 + 哈希**的后处理，遍历本身是串行的。
- 默认 `read_contents: true`（config.rs:57）→ 每个 ≤10 MB 的文件都 `fs::read` +
  `xxh3_64` + `String::from_utf8`。
- 每个文件的固定开销：`entry.path().to_path_buf()` → `fs::metadata()`（**一次额外
  syscall**）→ extension 转 `String` → `Language::from_path()` → 再 `to_path_buf()` 一次；
  且整条链是 `Box<dyn Iterator>`，**全程动态分发、无法内联**。

**实测对照**（同机 16 逻辑核 / NTFS，最优值）：

*热缓存小树 `D:\Documents\GitHub`*

| 方案 | 文件数 | 耗时 | 相对 ignore-8T |
|---|---|---|---|
| ignore WalkParallel 8T | 134,202 | **181.9 ms** | 1.00x |
| ignore WalkParallel 1T | 134,202 | 702.9 ms | 3.86x |
| fsindex 最小配置（不读内容、无 gitignore） | 134,202 | **8,273.8 ms** | **45.5x 慢** |
| fsindex 默认配置（读内容 + 哈希） | 3,804（gitignore 过滤后） | 3,482.6 ms | 19.1x 慢（且只覆盖 2.8% 的文件） |
| fsindex `files_parallel()` 默认 | 3,804 | 1,214.7 ms | 6.7x 慢（同上） |

*冷缓存大树 `C:\Users\16695`*

| 方案 | 文件数 | 耗时 |
|---|---|---|
| ignore 8T | 671,624 | **5,675.7 ms** |
| ignore 1T | 671,624 | 18,641.3 ms |
| ignore 8T + 每文件一次 `fs::metadata` | 671,624 | 14,995.6 ms（stat 单项成本 ≈ **14 µs/文件**） |
| fsindex 最小配置 | 671,631 | **60,241.3 ms**（10.6x 慢于 8T，3.2x 慢于 1T） |

**判读**：fsindex 的每文件 stat 只解释了约一半差距，另一半来自串行遍历 + 每文件 3 次堆
分配 + `Box<dyn Iterator>` 动态分发。它比 ignore **单线程**还慢 3.2 倍。默认配置更不可用：
为了找 ~30 个候选而读遍全盘文件内容。

**维护风险（叠加在性能之上）**：GitHub 仓库 `xandwr/fsindex` 已 404；crates.io 上
`documentation` 字段为空（无 docs.rs）；累计下载 497 次；未声明 MSRV。作为要分发的工具，
这个依赖风险偏高。

**结论**：`fsindex` 面向的是"给 LLM/RAG 建代码索引"（内容哈希、语言检测、符号解析、
增量 diff），cefscan 需要的是"以最低成本枚举出极少数候选文件"，两者目标相反。**不采用**。

### 若仍想摆脱 `ignore`，按推荐顺序

1. **自写遍历**（`std::fs::read_dir` + rayon / `std::thread::scope`）。cefscan 不需要
   gitignore、不需要 per-file stat（`entry.file_type()` 免费，Windows 上
   `DirEntry::metadata()` 来自 `WIN32_FIND_DATAW` 也无 syscall），砍掉这些就能比 `ignore`
   更快，且依赖为零、行为可控、便于 fixture 测试。**← 本项目采用的就是这条**（`walk.rs`）。
2. **`jwalk`**：基于 rayon 的并行遍历，Windows 上取元数据无需额外 stat。
3. **Windows 原生枚举**：`NtQueryDirectoryFile` 批量取 / `FSCTL_ENUM_USN_DATA`（USN 日志）
   —— Everything 的提速原理，但需要特权且实现复杂，可作为后续的可选后端。
4. **保留 `ignore`**：生态成熟、gitignore 规则齐全，是稳妥基线。

> 唯一值得从 fsindex 借鉴的是**增量状态**（`save_state` / `diff_with_state`）。但它的 diff
> 仍需先全量遍历一遍才能比对，**省不掉遍历本身** —— 而遍历恰恰是 cefscan 的成本主体，
> 所以这个特性对我们无效。

**原则**：先 profile 再优化。每一项优化都要有 benchmark 数字支撑才合入主干；上面四组
数字即为基线，后续优化项必须拿同机同目录的对比数据才能合入。历次对照结果记在
[`benchmark.md`](benchmark.md)，那里也写明了哪些改动**量不出来**以及为什么。
