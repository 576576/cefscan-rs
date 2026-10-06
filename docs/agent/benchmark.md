# 基准：代码评审前后的性能对比

> 基线 = `1dd7e52`（评审提交 `d19a312` 的父提交，也就是**评审前的最后一份代码**）；
> 对比 = 本轮评审落地收尾后的 HEAD。同机（16 逻辑核 / NTFS）、同目录、两个二进制**交替跑**取最优，
> 避免顺序漂移。改动逐项对应 [`review.md`](review.md) 的编号（那份文件已随条目全部落地而删除，
> 编号含义见下表的备注列）。
>
> 复现命令见文末 §4。

## 0. 结论先说

| 层面 | 结果 |
| --- | --- |
| 端到端 · 真实大树（4.3 万目录 / 22 个应用） | **1.00x，落在噪声里** |
| 端到端 · 真实中树（1030 目录 / 0 个应用） | **1.05x** |
| 端到端 · 合成候选密集树（2000 个应用 / 4000 次签名扫描） | **1.19x**（中位 1.22x） |
| 算法微基准 | `deduplicated_total` 最高 **8500x**，`drop_apps_nested_in_identified_roots` **10.7x**，`allows_path` **6.3x** |

**一句话**：这轮改动的收益集中在「应用数 / 候选数很大」时的**算法复杂度**与**每目录的固定开销**上；
普通用户扫描本机（几十个应用、几万目录）的体感几乎不变。真正的量级提升仍然来自
「用 Everything 而不是遍历」—— 那是 1~2 个数量级的差距，见 [`performance.md`](performance.md) §2。

## 1. 端到端（CLI 全流程，`--no-running`）

数字是 `cefscan --root <树> --no-running` 自报的 `耗时 N ms`，交替跑多轮取**最优**（中位一并列出，
用来看噪声有多大）。

| 树 | 规模（扫描时实测） | 评审前 | 现在 | 最优倍数 | 中位倍数 |
| --- | --- | --- | --- | --- | --- |
| **A** `C:\Users\16695` | 遍历 43223 个目录 / 22 个应用 / 30 个候选 | 2441 ms | 2432 ms | 1.004x | 1.000x |
| **B** `D:\Documents\GitHub` | 遍历 1030 个目录 / 0 个应用 | 21 ms | 20 ms | 1.05x | 1.05x |
| **C** 合成候选密集树 | 2001 个目录 / 2000 个应用 / 4000 次签名扫描 | 1746 ms | 1463 ms | **1.19x** | **1.22x** |

**怎么读这三行**：

- **A 没有变化是符合预期的**，不是"优化没生效"。这棵树只有 22 个应用，`deduplicated_total` 与
  `drop_apps_nested_in_identified_roots` 的 n 太小；`allows_path` 每个目录省 0.68 µs × 43223 ≈ **29 ms**，
  只占 2441 ms 的 1.2%，埋在 ±10% 的机器噪声里。9 轮的最优值差 9 ms（0.4%）。
- **B 的 1 ms 差**同理（1030 个目录 × 0.68 µs ≈ 0.7 ms），但这棵树小到能看清，所以 1.05x 是可信的。
- **C 才吃到了本轮的全部收益**：2000 个应用让 `deduplicated_total` 的 O(n²) 变 O(n log n)（≈ 235 ms → 0），
  4000 次签名扫描让 1 MiB 缓冲的复用（PERF-1）省下约 150 ms。

## 2. 微基准（release，3 轮取最优）

用临时 `#[cfg(test)]` 模块直接调被测函数（测完已删除），`cargo test --release -- --nocapture`。

| 被测 | 场景 | 评审前 | 现在 | 倍数 | 对应改动 |
| --- | --- | --- | --- | --- | --- |
| `filter::allows_path` | 2 万条深度 7 的路径 × 20 轮 | 0.8042 µs/次 | **0.1284 µs/次** | **6.3x** | PERF-7.3 |
| `scan::deduplicated_total` | n=2000（100 顶层 + 1900 嵌套） | 173.67 ms | **1.23 ms** | **141x** | PERF-7.1 |
| `scan::deduplicated_total` | n=2000（同级、互不嵌套） | 235.05 ms | **0.0275 ms** | **8547x** | PERF-7.1 |
| `group::drop_apps_nested_in_identified_roots` | n=4000（2000 已识别嵌套 + 2000 未识别） | 108.33 ms | **10.12 ms** | **10.7x** | PERF-7.2 |
| `group::drop_apps_nested_in_identified_roots` | n=60（现实规模） | 36.96 µs | 36.84 µs | 1.00x | PERF-7.2 |

三条改动各自为什么快：

- **`allows_path`（6.3x）**：原来每个路径要走三遍 `components()`（隐藏检查一遍、平台排除一遍、
  逐组件排除一遍），其中 Windows 的平台排除还对**每个组件**做一次 `to_ascii_lowercase()` 分配 `String`。
  现在合成一遍遍历，并且用 `[u8]::eq_ignore_ascii_case` 直接比字节，零分配。
  顺带删掉了一处完全冗余的判断（`include_hidden || !is_hidden(path)` 已被逐组件检查蕴含）。
- **`deduplicated_total`（141x / 8547x）**：原来是「每条都扫一遍全表」的 O(n²)。现在按 root 长度升序处理，
  只跟**严格更短**的已保留根比（`partition_point` 把 `kept` 切成"更短的一段"）—— 能包含别人的根一定更短，
  长度相同的根不可能互相包含。同级 2000 条的情形于是直接退化成一次排序。
- **`drop_apps_nested_in_identified_roots`（10.7x）**：同理，并且先用同样的办法把已识别根筛成「顶层根」
  （嵌套传递性保证只比顶层就够），未识别根只跟这几十个顶层根比，而不是跟全部已识别根比。
  n=60 那行说明**现实规模下这条本来就不是瓶颈**（36 µs），改动只是把最坏情况兜住。

### 2.1 一处刻意的语义收窄

长度剪枝隐含了一个前提：**两个长度相同、仅大小写或斜杠方向不同的根，不再被判为互相包含**。
`path_starts_with` 是大小写不敏感 + 斜杠方向不敏感的，所以旧实现会把 `c:\apps\outer` 和
`C:\apps\OUTER` 当成同一个目录。

判断是安全的，而且算修掉了一个误判：

- 同一个扫描里的所有根来自**同一个后端**，Windows 上不会出现只差大小写的两条；
- Linux / macOS 是大小写敏感文件系统，`/apps/foo` 与 `/apps/FOO` 本来就是两个目录，旧实现才是错的。

两条断言钉住了新行为：`scan.rs::equal_length_roots_are_never_treated_as_nested` 与
`group.rs::equal_length_roots_are_never_dropped_as_nested`。

## 3. 本轮改动逐项对照

| 编号 | 改动 | 有性能意图？ | 量到了吗 |
| --- | --- | --- | --- |
| P0-1 / P0-2 | `--sort` 三值 + 方向不再退化 | ✗（正确性） | — |
| PERF-1 | 签名扫描缓冲改为复用（1 MiB × 文件数 → 每 rayon 任务一份） | ✓ | C 树 ≈ 150 ms |
| PERF-2 | 进程路径查询的 64 KiB 缓冲提到循环外 | ✓ | ✗（约 200 个进程 × 64 KiB ≈ 数 ms，量级太小） |
| PERF-3 | `inspect_directory` 的 `sort_by_key` → `sort_by_cached_key` | ✓ | ✗（每候选目录少一半 `PathBuf` 分配，候选少时看不见） |
| PERF-4 | 结果收集不再 clone 每条 `AppInfo` | ✓ | ✗（分配次数层面） |
| PERF-5 | 遍历结束不再整体 clone 候选向量 | ✓ | ✗（同上） |
| PERF-6 | 条件变量去掉 1 ms 超时轮询 | ✓ | ✗（独立测不出来；8 线程从 8000 次/秒空转唤醒降到 0） |
| PERF-7.1 | `deduplicated_total` 去 O(n²) | ✓ | 微基准 141x / 8547x，C 树 ≈ 235 ms |
| PERF-7.2 | `drop_apps_nested_in_identified_roots` 去 O(n²) | ✓ | 微基准 10.7x（n=4000）；现实规模 1.00x |
| PERF-7.3 | `allows_path` 三遍遍历合一 + 去分配 | ✓ | 微基准 6.3x；A 树 ≈ 29 ms |
| FP-1 ~ FP-4 | 手写状态机改组合子 | ✗（可读性） | — |
| §五 / §七 | 类型收口、去死代码 | ✗ | — |

**诚实的部分**：PERF-3 / PERF-4 / PERF-5 / PERF-6 这四项都是"少分配 / 少唤醒"层面的改动，
在真实规模的扫描里**量不出来**，它们的价值是「n 变大时不至于先崩在这里」，以及让热路径上的
分配次数可解释。只有 PERF-1、PERF-7.* 这四项有能拿出手的数字。

## 4. 复现

```bash
# 基线 worktree（评审提交的父提交）
git worktree add ../cefscan-rs-baseline 1dd7e52
(cd ../cefscan-rs-baseline && cargo build --release --locked)

# 端到端：交替跑两个二进制，解析 stderr 里的「耗时 N ms」
for i in 1 2 3; do
  ../cefscan-rs-baseline/target/release/cefscan.exe --root C:/Users/$USER --no-running
  ./target/release/cefscan.exe                   --root C:/Users/$USER --no-running
done

# 合成候选密集树：2000 个应用目录，每个一个 libcef.dll（带真实签名）+ 一个 exe
# 微基准：把 #[cfg(test)] mod bench_tmp 追加到 scan.rs / group.rs / filter.rs，然后
cargo test --release -p cefscan-core --lib -- bench_tmp --nocapture
```

> 基线那棵树里的 `deduplicated_total` 签名与现在不同（旧版收 `&[AppInfo]`），
> 微基准要按各自的签名写一份。这正是 PERF-4 那次改动的一部分。
