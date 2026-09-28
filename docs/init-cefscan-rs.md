# cefscan-rs 实施计划

> 目标：从零编写一个 Windows 优先的 CEF / Chromium 内核应用扫描器，核心是 `cefscan` 命令行工具，GUI 用 Tauri 2 承载。
> 差异化重点：**扫描性能** 与 **工程可测试性**。
> 参考对象：`D:\Documents\GitHub\CefDetector`（C# / WinForms 原版）与 `D:\Documents\GitHub\CefDetector-rs`（Rust / egui 重写版）。

---

## 1. 参考资料盘点与结论

### 1.1 两个参考仓库对比

| 维度 | CefDetector (C#) | CefDetector-rs (Rust) |
| --- | --- | --- |
| 规模 | `Form1.cs` 254 行，单文件 | `src/**` 约 10.4k 行，15 个模块 |
| UI | WinForms（Button + Panel + 背景音乐） | egui + glutin + winit 自绘（`gui.rs` 2031 行） |
| 搜索 | 仅 Everything SDK（`Everything64.dll` P/Invoke） | trait 抽象 4 后端：ignore / plocate / Everything IPC / Spotlight |
| 类型识别 | 4 条签名：`cef_string_utf8_to_utf16`、`third_party/electron_node`、`register_atom_browser_web_contents`、`CefSharp.Internals`、`url-nwjs` | 同上 + Mini 分支（`napi_create_buffer`、`miniblink`），并带优先级 rank |
| 配置 | 无 | `config.rs` **3045 行**（含 GUI 像素级配置 + 带行列号的 TOML 诊断） |
| 输出 | 只有 GUI | CLI 支持 TOML / JSON / CSV（手写序列化器，无 serde_json） |
| 测试 | 无 | 各模块 `#[cfg(test)]`，fixture 用 tempdir 造 Mach-O / PE / `.app` 假文件 |

### 1.2 值得继承的（已被实践验证的确定性知识）

1. **三段式流水线**：候选发现 → 二进制签名扫描 → 按目录分组计量。后端只负责"发现候选"，检测逻辑与后端完全解耦（`src/search/backend.rs:36-46` 的 `CandidateSource` trait 注释即为设计契约）。
2. **候选文件名分类表**（`src/search/backend.rs:171-198`）：
   - `*_100_*.pak` → `Pak`（对应 `chrome_100_percent.pak`，Electron/CEF/Chrome 通用伴随文件）
   - `libcef.so` / `libcef.so.*` / `libcef.dll` / `libcef.dylib` / `Chromium Embedded Framework` / `Electron Framework` → `Cef`
   - `libnode.so[.*]` / `libnode.dll` / `libnode.dylib` → `Node`（MiniElectron / MiniBlink 线索）
3. **签名优先级 rank**（`src/search.rs:51-61`）：Electron 100 > Edge/Chrome 95 > NWJS 90 > CefSharp 80 > MiniElectron 75 > MiniBlink 70 > CEF 60。命中最高 rank 时（Electron）可提前结束该文件扫描。
4. **分块扫描 + 重叠**：1 MiB 块 + 64 B 重叠（`SIGNATURE_CHUNK_SIZE` / `SIGNATURE_OVERLAP`，`src/search.rs:22-23`），避免签名跨块被截断。
5. **Windows 进程路径必须归一化**（`src/search.rs:338-350`）：`\\?\` 前缀剥离、`\\?\UNC\` 还原为 `\\`、斜杠统一、整体小写比较。这是最容易出 bug 的地方，参考实现对它有专门单测。
6. **平台排除目录**：Windows 侧排除 `WinSxS` / `servicing` / `Recovery` / `System Volume Information`（`src/search/backend/ignore.rs:110-166`），且大小写不敏感但必须是**精确目录边界**（`WinSxSBackup` 不能被误伤）。

### 1.3 明确不继承的

- **不继承 3045 行的配置系统**。cefscan 只做扫描，配置面控制在 20 个键以内，用 `serde` + `Default` 派生 + 一层文件覆盖 + `--set k=v` 即可。
- **不继承 egui 自绘 GUI**。改用 Tauri 2 + Web 前端，把渲染复杂度移出 Rust。
- **不继承手写序列化器**。直接用 `serde_json` / `csv` / `toml`，换取正确性与 schema 稳定。
- **不继承 `AppInfo.app_type: String`**（`src/models.rs:4`）。改用 `#[non_exhaustive] enum AppKind` + `serde(rename_all = "snake_case")`，让输出 schema 可被机器安全消费。
- **不继承串行签名扫描**。参考实现的 `inspect_directory` 是单线程串行的（`src/search.rs:522-615`），这是本项目最主要的提速空间。

---

## 2. 目标与非目标

### 目标
- **G1**：`cefscan` CLI 能在 Windows 上全盘扫描，列出所有 CEF / Electron / NWJS / CefSharp / Edge / Chrome 应用，含路径、类型、磁盘占用、是否正在运行。
- **G2**：Everything 可用时秒级完成；不可用时自动回落到 `ignore` 并行遍历。
- **G3**：输出稳定、可脚本消费（JSON / CSV / TOML / 人类可读表格）。
- **G4**：核心逻辑可在**不触碰真实磁盘**的前提下 100% 确定性测试。
- **G5**：Tauri 2 GUI 提供渐进式结果流、按大小排序、点击定位到资源管理器。

### 非目标（本期不做）
- macOS `.app` bundle 解析、Spotlight、plocate（架构留接口，实现后置）
- 应用图标提取（后置 feature）
- 版本号识别（M7 之后的 stretch）
- 删除/清理 CEF 的能力（纯只读工具）

---

## 3. 总体架构

```
                 ┌──────────────────────────────────────┐
                 │         cefscan-core (lib)           │
                 │                                      │
  CandidateSource│  ① discover   ② classify   ③ group   │
   (trait) ─────►│                                      │
                 │  ④ size (rayon)  ⑤ running-proc      │
                 └───────────┬──────────────────────────┘
                             │ ScanEvent 流 / Vec<AppInfo>
              ┌──────────────┴───────────────┐
              ▼                              ▼
   ┌────────────────────┐        ┌──────────────────────────┐
   │  cefscan (CLI bin) │        │  cefscan-desktop (Tauri2)│
   │  clap + 输出格式化 │        │  #[tauri::command]       │
   └────────────────────┘        │  + React/TS 前端         │
                                 └──────────────────────────┘
```

### 3.1 Workspace 布局

```
cefscan-rs/
├── Cargo.toml                 # workspace 根，[workspace.dependencies] 统一版本
├── rust-toolchain.toml        # channel = "1.92.0"（与参考实现对齐），components: clippy, rustfmt
├── .github/workflows/{ci,release}.yml
├── crates/
│   ├── cefscan-core/          # 引擎库：唯一的知识沉淀处
│   │   ├── src/
│   │   │   ├── lib.rs         # 公开 API + prelude
│   │   │   ├── model.rs       # AppInfo / AppKind / ScanConfig / ScanStats
│   │   │   ├── error.rs       # thiserror：ScanError
│   │   │   ├── candidate.rs   # 候选分类 + CandidateSource trait
│   │   │   ├── backend/
│   │   │   │   ├── mod.rs
│   │   │   │   ├── walk.rs        # ignore 后端（全平台，回退用）
│   │   │   │   ├── everything.rs  # Windows Everything IPC（feature = "everything"）
│   │   │   │   └── memory.rs      # 测试替身：内存候选源（cfg(test) / feature = "testkit"）
│   │   │   ├── signature.rs   # 签名表 + 分块扫描器（对 impl Read 工作）
│   │   │   ├── inspect.rs     # 目录检查：挑可执行文件、打分
│   │   │   ├── group.rs       # 按 app root 分组、去重、父子包含消解
│   │   │   ├── size.rs        # 并行目录计量（rayon）
│   │   │   ├── process.rs     # 运行进程集合（Windows / Unix 分实现）
│   │   │   ├── filter.rs      # 排除规则（目录名 / 路径 / 回收站 / 平台目录）
│   │   │   └── scan.rs        # 编排：ScanOptions -> Vec<AppInfo> 或事件流
│   │   └── tests/             # 集成测试：fixture 目录树 + 快照
│   ├── cefscan-cli/           # 二进制名 cefscan
│   │   └── src/{main.rs, cli.rs, output.rs}
│   └── cefscan-desktop/       # Tauri 2 外壳
│       ├── ui/                # 手写 HTML/CSS/JS（无构建步骤，见 §8）
│       │   └── {index.html, main.js, styles.css}
│       └── src-tauri/
│           ├── src/{main.rs, lib.rs}
│           ├── icons/icon.ico
│           ├── capabilities/default.json
│           └── tauri.conf.json
├── docs/
│   ├── init-cefscan-rs.md     # 本文档
│   └── schema.md              # 输出 schema 契约（冻结后不得随意变更）
├── tools/
│   └── make_icon.py           # 生成 icons/icon.ico（纯标准库）
├── benchmarks/                # benchmark.ps1 / criterion benches
└── completions/               # clap 生成的 bash/zsh/fish/powershell 补全
```

> 说明：仓库名已定为 `cefscan-rs`（crate 名不带 `-rs` 后缀，遵循 `name-crate-no-rs`）。

---

## 4. 核心数据模型

```rust
// crates/cefscan-core/src/model.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Electron, Nwjs, CefSharp, Edge, Chrome, Cef, MiniElectron, MiniBlink, Unknown,
}

impl AppKind {
    /// 参考实现验证过的优先级；用于同一目录多签名冲突时取最强者。
    pub const fn rank(self) -> u8 { /* Electron=100 … Cef=60, Unknown=0 */ }
    pub const fn label(self) -> &'static str;
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AppInfo {
    /// 展示路径：优先可执行文件，其次应用根目录
    pub path: PathBuf,
    /// 用于计量与去重的根目录
    pub root: PathBuf,
    pub kind: AppKind,
    pub size: u64,
    pub running: bool,
    /// 命中的签名（诊断用，--verbose 才输出）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<&'static str>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ScanOptions {
    pub roots: Vec<PathBuf>,           // 空则自动推导（Windows: GetLogicalDrives）
    pub backend: Backend,              // Auto | Index | Filesystem
    pub exclude_dir_names: Vec<String>,// 默认 ["node_modules", "target", "$Recycle.Bin"]
    pub exclude_paths: Vec<PathBuf>,
    pub include_hidden: bool,
    pub follow_symlinks: bool,
    pub walk_threads: usize,           // 0 = 自动
    pub scan_threads: usize,           // 签名扫描并行度
    pub size_threads: usize,
    pub timeout: Option<Duration>,     // Everything IPC 超时
}

#[derive(Debug, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Backend { Auto, Index, Filesystem }
```

**为什么用 enum 而不是 String**：CLI 输出是公开契约，`"Electron"` / `"electron"` 大小写不一致会让下游脚本崩溃；enum + `serde(rename_all)` 从类型层面杜绝。（对应 `type-no-stringly` / `serde-rename-all`）

---

## 5. 流水线设计（五阶段）

### 阶段 1 — 候选发现（CandidateSource trait）

```rust
pub trait CandidateSource {
    /// 只负责"发现"，不做二进制检查、不做分组、不算大小。
    fn find_candidates(&self, opts: &ScanOptions) -> Result<Vec<Candidate>, ScanError>;
}

pub struct Candidate {
    pub path: PathBuf,
    pub kind: CandidateKind,   // Pak | Cef | Node
}
```

| 后端 | 平台 | 状态 | 备注 |
| --- | --- | --- | --- |
| `walk` (`ignore` crate) | 全平台 | M1 必做 | `WalkBuilder::build_parallel()`，`filter_entry` 提前剪枝 |
| `everything` | Windows | M4 必做 | 见 §6 |
| `memory` | 全平台 | M1 必做 | 测试替身，让整条流水线可脱离磁盘测试 |
| `plocate` / `spotlight` | Linux / macOS | 后置 | trait 已留口，M7 后补 |

`Auto` 语义：先试索引后端，**失败即回落**到 `walk`，并把实际使用的后端名写入 stderr 一行（`cefscan-search-backend=everything`），便于排障——这个诊断约定直接沿用参考实现（`src/search/backend.rs:62-73`）。

### 阶段 2 — 签名扫描（重点提速区）

沿用参考实现验证过的签名表，但重构为**可并行、可复用 Finder**：

```rust
pub struct SignatureScanner { /* 预构建的 memchr::memmem::Finder，避免每次 find 重建状态 */ }

impl SignatureScanner {
    /// 对任意 Read 工作 —— 测试里喂 Cursor<Vec<u8>> 即可，无需落盘。
    pub fn scan<R: Read>(&self, reader: &mut R, flavor: Flavor) -> io::Result<Option<(AppKind, &'static str)>>;
}
```

| flavor | 签名（memchr 子串） | 判定 |
| --- | --- | --- |
| Standard | `third_party/electron_node`、`register_atom_browser_web_contents` | Electron |
| Standard | `url-nwjs` | Nwjs |
| Standard | `CefSharp.Internals` | CefSharp |
| Standard | `cef_string_utf8_to_utf16` | Cef |
| Mini | `napi_create_buffer` | MiniElectron |
| Mini | `miniblink` | MiniBlink |
| 文件名 | `msedge.exe` / `msedge_proxy.exe` / `chrome.exe` | Edge / Chrome（短路返回，无需读文件） |

要点：
- 先读 4 字节 magic 判断 ELF / PE (`MZ`) / Mach-O（含 8 种 magic，见 `src/search.rs:447-458`），不是可执行格式直接跳过，省掉大量无效 IO。
- 1 MiB 分块 + 64 B 重叠；命中 Electron（最高 rank）立即 break。
- 参考实现是**逐文件串行**扫描（`src/search.rs:531-604`）；本项目改为 rayon `par_iter` 并行，见 §12。

### 阶段 3 — 目录检查与分组

- 一个目录只做一次 `inspect`（`HashMap<PathBuf, DirInspection>` 缓存，参考实现同思路 `src/search.rs:617-627`）。
- 可执行文件打分（沿用 `src/search.rs:487-520` 的权重并显式列出）：`.exe`/`.appimage` +40、无扩展名 +30、与目录同名 +20、名字含 `web`/`browser`/`cef` +30，基础分 10。
- 排除噪声可执行文件：`unins*`、`*setup*`、`*report*`、`chrome-sandbox`、`crashpad_handler`。
- 目录内无命中时**向上一层**再试一次（Electron 常见 `resources/` 布局）。
- 去重：以 `root` 为 key，`BTreeMap` 保证确定性；冲突时 rank 高者胜、同 rank 时"有可执行文件的"胜过"纯目录"。
- 父子包含消解：若 `Unknown` 的根是另一个已识别根的子路径，丢弃（`src/search.rs:811-819`）。

### 阶段 4 — 磁盘计量

- rayon 并行 `dir_size`，worker 数上限 `min(size_threads, roots.len())`，参考实现用 `std::thread::scope` + `AtomicUsize` 取任务（`src/search.rs:202-238`），本项目直接换 rayon `par_iter` 更简洁。
- **硬链接去重**：Linux/macOS 用 `(dev, ino)` 访问集（`src/search.rs:184-190`）；**Windows 需额外处理**——`WinSxS` 的硬链接会让总量虚高，M5 评估用 `GetFileInformationByHandle` 的 `nFileIndexHigh/Low` + `dwVolumeSerialNumber` 做去重（这是参考实现没有做、且确实存在的计量偏差）。
- **总量语义要写进 README**：各 root 若有嵌套，"总占用"不等于各条之和。参考实现只做了粗糙的父子消解，本项目在 `ScanStats` 里同时给出 `total_bytes`（去重后的并集口径）与 `sum_bytes`（列表求和口径），避免用户困惑。

### 阶段 5 — 运行进程检测（Windows）

- `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` → `Process32FirstW/NextW` → `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` → `QueryFullProcessImageNameW`（参考实现 `src/search.rs:266-336`）。
- 受保护进程 `OpenProcess` 会失败，**必须静默跳过**而非 panic。
- 路径归一化：`\\?\UNC\Server\Share\x` → `\\server\share\x`，`\\?\C:\a/b.exe` → `c:\a\b.exe`，统一小写后比对；再 `fs::canonicalize` 兜底比对一次。

---

## 6. Everything IPC 后端（Windows，M4）

这是 Windows 上唯一能秒级出结果的路径，也是参考实现里最脆的一段（`src/search/backend/everything.rs` 403 行 + `everything_protocol.rs` 209 行）。要点：

1. Everything 必须**正在运行**且**非精简版**（Lite 无 IPC）。
2. 探测顺序要兼容 1.4/1.5 alpha 等不同实例（窗口类名不同，见 `everything.rs:83-150` 的 `find_everything_window`）。
3. 自建隐藏窗口接收 `WM_COPYDATA` 回复，需要**两个超时**：发送超时与回复超时（配置项 `send_timeout_ms` / `reply_timeout_ms`）。
4. 查询结果一次拿全路径（`EVERYTHING_REQUEST_PATH | EVERYTHING_REQUEST_FILE_NAME`）。
5. 查询串：参考 C# 原版是两次查询 `_percent.pak` 与 `libcef`（`Form1.cs:230-244`）；CefDetector-rs 合并为一次复合查询。本项目用**一次复合查询**减少 IPC 往返。
6. **失败必须可回落**：IPC 失败 → `walk` 后端，绝不因此让整个扫描失败。

### 6.1 落地后的协议实测（已真机验证）

本机装了 Everything 1.4（`EVERYTHING_TASKBAR_NOTIFICATION` 类）后实测，把
计划阶段没写死的部分钉下来：

- **查询体**：5 个 `u32`（回复窗口句柄、回复 id、search flags、offset、max results）
  + UTF-16LE NUL 结尾的搜索串，`COPYDATASTRUCT.dwData = 2`。
- **回复体**：头部是 **7 个 `u32`（28 字节）**，第 5 个（偏移 20）是条目数；
  随后每条目 3 个 `u32`（flags、文件名偏移、路径偏移），偏移基准是**回复体起点**。
  字符串区紧跟在条目数组之后，即 `28 + count * 12`。
  实测校验：`libcef` 查询返回 54 条 → 数据区从 676 = `28 + 54*12` 开始，吻合。
- 回复的 `dwData` 会带回我们发送的回复 id，不是固定常量。
- 搜索串 `file: <_100_|libcef|libnode|"Chromium Embedded Framework">` 一次覆盖
  四类候选，实测全盘返回 229 条。

**踩过的两个坑**（都已修，并补了回归测试）：

1. **`PCWSTR` 少写 NUL 结尾**。`to_wide()` 返回的 `Vec<u16>` 没有终止符，却被直接
   传给 `FindWindowW` 和 `WNDCLASSEXW.lpszClassName`。Win32 不会报错，只会静默匹配
   不上，表现成"Everything is not running"。现在拆成 `to_wide`（构造 `OsString` 用）
   和 `to_wide_z`（给 Win32 用），并有测试钉死这个区别。
2. **索引后端没有应用 `--root` 和排除规则**。索引是全局的，而 `Filter` 原本只服务
   遍历阶段的剪枝，于是 `--backend index --root C:\Users\me` 会把整个磁盘的结果吐出来，
   还会带上 `WinSxS`。现在在结果侧用 `Filter::allows_path` 再筛一遍（`allows_dir`
   保留为它的别名，语义上遍历筛目录、索引筛文件）。

**两个后端口径一致**：`--root C:\Users\16695` 下 index 与 filesystem 都是
21 应用 / 9.9 GiB / 29 候选；index 391 ms，filesystem 1581 ms（全盘 index 931 ms）。

> 另外 `everything` 必须是 core 的**默认 feature**。它只控制 `scan/everything.rs`
> 是否编译；设成可选时 `cargo test` 默认不会编译那个模块的测试，等于 IPC 编解码
> 完全没有覆盖——上面第 1 个坑就是这么漏过去的。

---

## 7. CLI 设计

```
cefscan [OPTIONS]                 # 默认：扫描 + 人类可读表格输出
cefscan scan [OPTIONS]            # 显式子命令
cefscan config paths|show         # 配置路径 / 生效配置
cefscan completions <shell>       # 生成补全
cefscan benchmark [--rounds N]    # 自测耗时与峰值内存

选项：
  -f, --format <table|json|csv|toml|ndjson>   默认 table
  -o, --output <FILE>                         写文件（默认覆写，--no-overwrite 时冲突即报错）
      --root <DIR>               可重复；不传则全盘
      --backend <auto|index|filesystem>
      --exclude-dir <NAME>       可重复
      --exclude-path <PATH>      可重复
      --kind <electron|nwjs|...> 可重复，过滤类型
      --running-only
      --min-size <1GB|512MB>
      --sort <size|path|kind|running>   --desc/--asc
      --threads <N>
  -v, --verbose                  输出命中的签名证据 + 实际后端
  -q, --quiet
```

退出码：`0` 成功（含 0 结果）、`1` 扫描失败、`2` 参数/配置错误。

**输出 schema 冻结**：`docs/schema.md` 定义 JSON 字段名与类型，一旦发布 1.0 不得破坏；新增字段只能追加且可选。CI 里对 fixture 结果做**快照测试**（`insta`），格式一改测试就红。

---

## 8. Tauri 2 GUI 设计（M6）

> **实施偏差（已落地）**：原计划用 React 19 + TS + Vite，实际改为**手写原生
> HTML/CSS/JS + `withGlobalTauri`**，彻底去掉 Node 工具链。理由是 GUI 只有
> 一张表格加一条工具条，引入打包器带来的收益抵不过成本：构建要装几百 MB 的
> node_modules、前端产物还要跟 Rust 产物分别管理，而本项目的硬约束是
> "一条 `cargo build --release` 出两个 exe"。列表规模用「只渲染前 500 条 +
> rAF 合并重绘」解决，不需要虚拟滚动。

- **前端**：`crates/cefscan-desktop/ui/`，三个文件，无构建步骤，由 Tauri 直接内嵌。
  通过 `app.withGlobalTauri = true` 拿到 `window.__TAURI__.core.{invoke, Channel}`。
- **通信**：
  - `#[tauri::command] async fn scan_apps(channel, request)` → 在 `tauri::async_runtime::spawn_blocking` 里跑 core（CPU 密集，绝不能堵住 async 运行时）。
  - 结果通过 Tauri 2 `Channel` **流式推送**给前端，而不是等全部扫完。
  - 关键点：`scan_streaming` 的回调是在**计量阶段**边算边发的
    （`sizes_parallel_each` 每算完一个根目录就回调一次），不是最后统一发一遍。
    应用体积差异很大（小的几十 MB、大的几个 GB），逐个发射能让用户立刻看到结果。
    代价是发射顺序不确定，所以 `scan_streaming` **不排序**，排序由 `scan()` /
    前端各自负责（`sort_apps` 是共用实现）。
  - 事件类型：`ScanEvent::{Started, Item(AppRow), Done{...}, Error{message}}`，serde 用 `tag = "type"` 打标签。
- **能力**：图标列 + 名称列 + 类型 + 占用 + 运行 + 路径（可排序）、类型筛选、运行中高亮、
  点击行展开完整路径并在资源管理器中定位（`explorer /select,"path"`，注意路径带空格必须加引号，
  参考实现有对应单测 `src/search.rs:875-883`）。
- **边界**：GUI 不复制任何检测逻辑，只做 `cefscan-core` 的消费者；core 不依赖 Tauri。
- **构建**：`bundle.active = false`，只要裸 exe 不要安装包；`main.rs` 上
  `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` 去掉控制台。
  `tauri-build` 生成 Windows 资源文件需要 `icons/icon.ico`，由 `tools/make_icon.py`
  生成并入库。运行期依赖系统自带 WebView2（Win10/11 默认已装）。

### 8.1 后端显示名、名称列与图标列（落地补充）

**后端显示名**。`ScanStats.backend` 是 `&'static str`，值只有两种：

| 后端 | 显示名 | 来源 |
| --- | --- | --- |
| 文件系统遍历 | `cefscan` | `scan::FILESYSTEM_BACKEND` 常量 |
| 索引 | **实际服务名**（如 `Everything`） | `scan/everything::SERVICE_NAME` 常量 |

索引后端返回服务名而不是笼统的 `index`，是为了以后接 plocate / Spotlight 时
显示名能自动跟着变，前端和 CLI 都不用改。GUI 汇总区、CLI 的 stderr 摘要都直接
读 `stats.backend`，所以改后端名只需要动 core 里那两个常量。

**名称列**：`cefscan_core::display_name(path)`（`crates/cefscan-core/src/naming.rs`）。
扫描结果里的 path 是"最能代表这个应用的那个文件或目录"，直接当名字没法看
（`...\Microsoft VS Code\Code.exe` → "Code"，`...\Edge\Application\154.0.4258.37\msedge.exe` → "msedge"）。
启发式是**纯字符串**的（不碰文件系统，因此好测）：从所在目录往上走最多 6 层，
跳过版本号目录（`154.0.4258.37`、`app-3.6.6`、`office6`）和通用目录名
（`Application`/`Bin64`/`runtime`/`resources`…），取第一个有意义的段；撞到
用户/系统目录（`Programs`、`LocalAppData`、`steamapps`…）就停，退回文件名。
`is_version_like` 的判据是"剥掉前导字母和分隔符后剩下纯数字+点/横线/下划线"——
这样 `BeamNG.drive`（剥完是空）和 `360se6`（含字母）不会被误判成版本号。
测试里有一张 11 条真实路径的期望值表，改启发式先看那张表。

**图标列**：`crates/cefscan-desktop/src-tauri/src/icon.rs`，链路是
`SHGetFileInfoW(SHGFI_ICON|SHGFI_LARGEICON)` → `HICON` → `GetIconInfo` 拆出彩色位图与掩码
→ `GetDIBits` 取 32bpp 自顶向下 BGRA → 补 alpha → PNG → `data:image/png;base64,…`。

三个实现选择：

- **用 `GetDIBits` 而不是 `DrawIconEx` 画进 DIB**：前者拿到的是位图原始像素，行为确定；
  后者是否保留 32bpp 图标的 alpha 通道取决于具体 GDI 实现。代价是老式图标要自己补
  alpha —— 判据是"彩色位图 alpha 全为 0"，这时改用掩码位图（白=透明、黑=不透明）。
  纯单色图标（`hbmColor` 为空）直接放弃，返回 `None`，前端留空格。
- **结果按路径缓存**（`OnceLock<Mutex<HashMap>>`）：同一个 exe 在列表里可能重复出现，
  而且每次 `SHGetFileInfoW` 都要碰一次 shell。失败也缓存，免得反复问。
  `SHGetFileInfoW` 要求线程先 `CoInitializeEx`，用 thread-local 挡一下重复初始化。
- **取图标这一段全局串行**（`imp::capture` 里一把 `Mutex<()>`）：`SHGetFileInfoW`
  **不能并发调用**。4 线程同时对同一个 exe 调用，240 次里有 3 次直接返回 0（拿不到
  `HICON`）。失败点在 shell 调用本身——同一轮实测里 `GetIconInfo` / `GetDIBits`
  都是 0 次失败，所以不是我们销毁句柄的问题。**这个并发在真实使用中一定会发生**：
  GUI 的图标提取跑在 rayon 工作线程上（`sizes_parallel_each` 的并行回调里），
  不加锁的表现是界面上偶发少一个图标。加锁后同样并发跑 0 失败；PNG 编码在锁外做。
  代价可以忽略：结果本来就按路径缓存，一次扫描最多几十个不同的 exe。

`icon.rs` 自带四条单测（拿测试进程自己的 exe 当样本）：PNG 签名与正方形尺寸、
**解出来必须有非透明像素**（防 alpha 补错导致整列空白格）、不存在的路径返回 `None`、
**4 线程并发提取同一个 exe 不许失败**（就是上面那个并发 bug 的回归测试；它直接打
`imp::extract` 而不是 `data_url`，否则会被结果缓存挡住、走不到 shell 调用）。

**路径列折叠**：按**分隔符切段**折叠，不是按字符数切——前面只留「根 + 3 层目录」
（`PATH_HEAD_SEGMENTS`），中间省略号，后面只留文件名，这样尾部一定是完整的文件名：

```
C:\Users\16695\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe
  → C:\Users\16695\AppData\…\WorkBuddy.exe
```

盘符（`C:`）和 UNC 的空段都算"根"，不占目录层数，所以 `\\server\share\dir\sub\file.exe`
折成 `\\server\share\…\file.exe`。折完不比原文短就返回原文。点整行展开完整路径。

表格用 `table-layout: fixed` + `<colgroup>` 固定各列宽度、路径列吃剩余空间，
这样折叠后的文本不会再被 CSS 的 `text-overflow` 二次截断（否则尾部会被吃掉）。

---

## 9. 性能专项（差异化 1）

| 措施 | 说明 |
| --- | --- |
| ① 并行签名扫描 | 参考实现串行（`src/search.rs:531`）。改为 rayon `par_iter` 扫描候选目录/文件。预期 4–8 核上有明显收益。 |
| ② 预构建 `memmem::Finder` | 每个签名的 `Finder` 构建一次复用，而非每次 `memchr::memmem::find` 重建。 |
| ③ 消除热路径分配 | 参考实现在遍历回调里做 `to_string_lossy().into_owned()`（每个候选一次分配）。本项目候选阶段全程持有 `PathBuf`/`OsString`，只在最终输出时转 `String`。文件名匹配用 `OsStr` 字节比较 + ASCII 小写归一化，不建临时 `String`。 |
| ④ 目录级短路 | Edge/Chrome 靠文件名直接判定，完全不读文件内容；`unins*/setup*/report*/chrome-sandbox/crashpad_handler` 直接跳过。 |
| ⑤ 提前剪枝 | `ignore` 的 `filter_entry` 在目录层就砍掉 `node_modules`、`WinSxS`、`$Recycle.Bin`，比事后过滤省掉整个子树遍历。 |
| ⑥ 线程本地缓冲 | 遍历线程各自持有 `Vec<Candidate>`，结束再 `extend` 合并，避免全量共享 `Arc<Mutex<Vec>>` 的锁竞争。 |
| ⑦ 减少 stat | 复用 `ignore::DirEntry` 已有的 `file_type()`/元数据，不额外 `fs::metadata`。 |
| ⑧ 可选 mmap | 大文件签名扫描用 `memmap2`（feature 门控），避免 1 MiB 缓冲的反复 read 系统调用。需实测是否有收益再合入。 |
| ⑨ 基准内建 | `cefscan benchmark --rounds 5` 输出 elapsed mean/min/max + 峰值 RSS；`benchmarks/benchmark.ps1` 对齐参考实现口径（scan / 冷启动）。 |

### 9.1 实测基线（本机 16 逻辑核 / NTFS / ignore 0.4.33）

用 `benchmarks/bench-ignore` 实测（3 次取最优，单位 ms）：

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

1. **并行遍历的天花板是 4–5x，不是线性**。冷缓存（IO 延迟可被重叠）能吃到 16 线程的红利；热缓存下 8 线程就见顶，12 线程以上反而变慢。→ `walk_threads` 默认取 `min(cpu, 8)`，**不要**跟随 `ignore` 自己的默认（`min(cpu, 12)`），并暴露 `--threads` 让用户按机器调。
2. **遍历吞吐**：热缓存 16 万 → 65.9 万条目/秒；冷缓存 5.9 万 → 31.7 万条目/秒。整盘 300 万条目的冷扫描，单线程约 50 s、8–12 线程约 10 s 量级。
3. **签名扫描是内存带宽瓶颈，不是 CPU**：单线程已 9.9 GiB/s，8 线程 3.81x 后 16 线程回落（超线程 + 带宽饱和）。真实场景里它更受**磁盘读取**限制，所以并行化有效但要配合批量顺序读。
4. **最重要的判断**：把遍历从 1x 优化到 4x，仍远不如**根本不遍历**。Everything 是索引查询（毫秒级），`ignore` 是穷举（秒到十秒级）——两者差 1~2 个数量级。所以优先级恒为：**Everything 后端 > 遍历调参 > 签名并行**。

### 9.2 fsindex 评估（结论：不适合 cefscan，已实测否决）

曾考虑用 `fsindex` 0.3.1 替代 `ignore`，实测后否决。先说它是什么：

**它是建在 `ignore` 之上的"代码索引库"，不是遍历替代品。** `Cargo.toml` 显式依赖 `ignore = "0.4"`，所以"不用 ignore"实际上做不到，只是把它降为传递依赖，同时额外拉进 `notify 8` / `rayon` / `serde_json` / `thiserror 2` / `xxhash-rust`。

**遍历部分的关键实现**（`src/indexer.rs`）：

- `build_walker()` 返回的是 `builder.build()`（:460）——**单线程 `Walk`**，没有 `build_parallel()`。
- `files_parallel()` 源码注释原文："*Collect paths first (can't parallelize the walk itself easily)*"（:257）——rayon 只用于**读内容 + 哈希**的后处理，遍历本身是串行的。
- 默认 `read_contents: true`（config.rs:57）→ 每个 ≤10 MB 的文件都 `fs::read` + `xxh3_64` + `String::from_utf8`。
- 每个文件的固定开销：`entry.path().to_path_buf()` → `fs::metadata()`（**一次额外 syscall**）→ extension 转 `String` → `Language::from_path()` → 再 `to_path_buf()` 一次；且整条链是 `Box<dyn Iterator>`，**全程动态分发、无法内联**。

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

**判读**：fsindex 的每文件 stat 只解释了约一半差距，另一半来自串行遍历 + 每文件 3 次堆分配 + `Box<dyn Iterator>` 动态分发。它比 ignore **单线程**还慢 3.2 倍。默认配置更不可用：为了找 ~30 个候选而读遍全盘文件内容。

**维护风险（叠加在性能之上）**：GitHub 仓库 `xandwr/fsindex` 已 404；crates.io 上 `documentation` 字段为空（无 docs.rs）；累计下载 497 次；未声明 MSRV。作为要分发的工具，这个依赖风险偏高。

**结论**：`fsindex` 面向的是"给 LLM/RAG 建代码索引"（内容哈希、语言检测、符号解析、增量 diff），cefscan 需要的是"以最低成本枚举出极少数候选文件"，两者目标相反。**不采用**。

**若仍想摆脱 `ignore`，按推荐顺序**：

1. **自写遍历**（`std::fs::read_dir` + `rayon`/`std::thread::scope`）。cefscan 不需要 gitignore、不需要 per-file stat（`entry.file_type()` 免费，Windows 上 `DirEntry::metadata()` 来自 `WIN32_FIND_DATAW` 也无 syscall），砍掉这些就能比 `ignore` 更快，且依赖为零、行为可控、便于 fixture 测试。
2. **`jwalk`**：基于 rayon 的并行遍历，Windows 上取元数据无需额外 stat，通常快于 `ignore`。
3. **Windows 原生枚举**：`NtQueryDirectoryFile` 批量取 / `FSCTL_ENUM_USN_DATA`（USN 日志）——Everything 的提速原理，但需要特权且实现复杂，可作为 M7 后的可选后端。
4. **保留 `ignore`**（现状）：生态成熟、gitignore 规则齐全、已实测 4–5x 加速，是稳妥基线。

> 唯一值得从 fsindex 借鉴的是**增量状态**（`save_state` / `diff_with_state`）。但它的 diff 仍需先全量遍历一遍才能比对，**省不掉遍历本身**——而遍历恰恰是 cefscan 的成本主体，所以这个特性对我们无效。

**原则**：先 profile 再优化（`perf-profile-first`）。每一项优化都要有 benchmark 数字支撑才合入主干；上面四组数字即为基线，M5 的优化项必须拿同机同目录的对比数据才能合入。

---

## 10. 工程与可测试性专项（差异化 2）

### 10.1 测试金字塔

| 层 | 做法 |
| --- | --- |
| 单元 | 每个模块 `#[cfg(test)] mod tests`。签名扫描器对 `Cursor<Vec<u8>>` 工作 → 构造假 PE/ELF/Mach-O 字节即可测，无需真实二进制。 |
| 夹具（fixture） | `crates/cefscan-core/tests/fixtures/` 提供**可生成的假应用树**构造宏：Electron 目录、NWJS 目录、纯 CEF 目录、Edge 目录、以及**负样本**（只有 `unins000.exe` 的目录、`node_modules` 内的 decoy）。测试在 tempdir 里造树 → 扫 → 断言。RAII `Drop` 清理（`test-fixture-raii`）。 |
| 替身 | `backend::memory::MemorySource` 实现 `CandidateSource` → 整条流水线可在无磁盘情况下端到端测试。 |
| 快照 | `insta` 对 JSON/CSV/TOML 输出做快照，格式变更立刻可见。 |
| 属性测试 | `proptest`：路径归一化（大小写/UNC/斜杠）、CSV/JSON 转义（引号、换行、控制字符、非 UTF-8）。 |
| 集成 | `assert_cmd`：跑真实 `cefscan --root <tempdir> --format json`，断言退出码与 schema 合法。 |
| 并发 | 涉及共享状态处（任务窃取、结果合并）用 `loom` 验证；或用固定线程数 + 重复运行保证确定性。 |

### 10.2 确定性要求（写进 CONTRIBUTING）

- 输出必须**按路径排序**后再序列化，禁止依赖 `HashMap` 迭代顺序。
- 任何测试不得访问真实全盘；需要真实路径的用例一律 `#[ignore]` 并标注手工运行方式。
- 时间/内存相关的断言只出现在 `benchmark` 子命令里，不进单测。

### 10.3 CI（`.github/workflows/ci.yml`）

**已落地**。三个 job，触发条件是 push 到 main、打 `v*` tag、PR、手动：

| job | 平台 | 做什么 |
| --- | --- | --- |
| `lint` | ubuntu | `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、feature 组合矩阵 |
| `test` | windows + ubuntu | `cargo test --workspace --locked`（Windows 上额外覆盖 `cefscanw` 的图标提取测试，那些是 `cfg(windows)` 的） |
| `build` | windows + ubuntu | `cargo build --release --locked --workspace` → 收成 `dist/` → `upload-artifact` |

要点与坑：

- **`build` 依赖 `lint` + `test`**，两者都绿才出产物。
- **Linux 每个 job 都要装 webkit 开发包**（`libwebkit2gtk-4.1-dev`、`librsvg2-dev`）。
  即使只跑测试也要装：`cefscanw` 在 workspace 里，`cargo test --workspace` 会编译它。
  没有用 `tray-icon` 特性，所以不需要 `libappindicator3-dev`。
  **`ubuntu-22.04` 不行**——它只有 webkit2gtk-4.0，Tauri 2 要 4.1。
- **产物里带 README + LICENSE**，下载下来就是一个自包含目录；`build` 之后跑一次
  `cefscan --version` / `--help` 当冒烟，`cefscanw` 是 GUI 不在 CI 里启动。
- **feature 组合矩阵**（`--no-default-features` 的四种组合）单列一步，防止
  `serde` / `everything` 悄悄退化成"其实必须开"。
- `--locked` 全用上，保证 CI 与 `Cargo.lock` 一致。
- 平台矩阵只出 **x86_64**。aarch64 的话：Linux 侧要交叉编译整套 webkit，成本高，
  更好的做法是用 `ubuntu-24.04-arm` runner 单开一个 job；Windows 侧交叉编译
  `aarch64-pc-windows-msvc` 可行（`.cargo/config.toml` 里已经留了 crt-static 配置）。

**尚未做**（原计划里有，按优先级排）：

- `cargo llvm-cov` 覆盖率，core crate 门槛先定 70%。
- `cargo deny check`（license + advisory）。
- `cargo miri test` 跑 `unsafe`（Windows FFI）——注意 miri 跑不了 Win32 FFI，
  实际能覆盖的只有纯逻辑部分。`// SAFETY:` 注释已经在写了。
- `cargo +1.92.0 check` 显式验 MSRV。现在靠 `rust-version` 字段兜底
  （toolchain 低于该版本时 cargo 直接报错），CI 用的是 `@stable`。
- tag 触发时自动建 GitHub Release 并附产物（属于 M7）。

### 10.4 Cargo profile（release）

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true
```

> 参考实现用 `opt-level = "z"`（体积优先）。本项目是扫描器，IO 与 CPU 都吃紧，选速度优先；若后续在意体积再评估。

---

## 11. 依赖清单

| 用途 | crate | 备注 |
| --- | --- | --- |
| CLI 解析 | `clap`（derive + `cargo` feature 读版本） | 顺带生成 shell 补全 |
| 目录遍历 | `ignore` | 成熟、并行、gitignore 规则齐全 |
| 数据并行 | `rayon` | 签名扫描 + 体积统计 |
| 子串搜索 | `memchr` | `memmem::Finder` 预构建 |
| 序列化 | `serde` / `serde_json` / `toml` / `csv` | serde 对 lib 做成 feature 可选 |
| 错误处理 | `thiserror`（core）/ `anyhow`（CLI） | 符合 `err-thiserror-lib` / `err-anyhow-app` |
| 日志 | `tracing` + `tracing-subscriber` | lib 只打点，订阅器由 CLI/GUI 安装 |
| Windows FFI | `windows-sys`（`Win32_System_Diagnostics_ToolHelp`、`Win32_Storage_FileSystem`、`Win32_UI_Shell`） | 直接 FFI，不引重量级封装 |
| 测试 | `insta`、`proptest`、`assert_cmd`、`tempfile`、`criterion` | dev-dependencies |
| GUI | `tauri` 2、`serde`、`tauri-plugin-*` | 独立 crate，不进 core |

**feature 设计（严格 additive）**：`default = ["everything"]`（Windows 上默认开），可选 `everything`、`mmap`、`serde`、`testkit`。

---

## 12. 里程碑与验收

| 里程碑 | 内容 | 验收标准 |
| --- | --- | --- |
| **M0 骨架** | workspace、crate 划分、`rust-toolchain.toml`、CI（fmt/clippy/test）、`.gitignore`、README 骨架、LICENSE | `cargo clippy -D warnings` 干净；CI 三平台绿 |
| **M1 候选发现** | `model.rs`、`error.rs`、`candidate.rs`、`backend/walk.rs`、`backend/memory.rs`、`filter.rs` | `MemorySource` 驱动的分类测试全绿；`walk` 后端能在指定 root 找出 fixture 里的 `libcef.dll` |
| **M2 签名扫描** | `signature.rs`、`inspect.rs`；签名表 + rank + 分块重叠 + magic 过滤 | 分块边界用例、跨块强签名用例、8 种 Mach-O magic 用例全绿（参考实现对应用例见 `src/search.rs:941-981`） |
| **M3 分组计量 + CLI** | `group.rs`、`size.rs`、`process.rs`、`scan.rs`、CLI（table/json/csv/toml） | `cefscan --root <fixture> --format json` 输出符合 `docs/schema.md`；运行进程高亮在 Windows 实测有效；快照测试通过 |
| **M4 Everything 后端** | `backend/everything.rs`、IPC 协议、超时、自动回落 | 装了 Everything 的机器上秒级出结果；未装/精简版时自动回落且不失败；`--verbose` 打印实际后端 |
| **M5 性能专项** | rayon 并行签名扫描、预构建 Finder、热路径去分配、`cefscan benchmark`、benchmark.ps1 | 输出 mean/min/max + 峰值 RSS；与 M4 基线对比有可量化提升并写入 README |
| **M6 Tauri 2 GUI** | `cefscan-desktop`、React 前端、流式 Channel、虚拟列表、资源管理器定位 | 冷启动 < 1.5 s；扫描过程中列表渐进增长不卡 UI；点击能正确定位 |
| **M7 发布工程** | Release workflow（tag `v*`）、NSIS 安装包、shell 补全、`docs/schema.md` 冻结、`completions/` | 打 tag 产出可安装产物；版本号与 `Cargo.toml` 一致（CI 校验） |

**后置**：Linux `plocate` 后端、macOS Spotlight + `.app` bundle、图标提取（`pelite`）、CEF/Electron 版本号识别。

**当前进度**：M0–M6 已落地并真机验证——两个 exe（`cefscan.exe` / `cefscanw.exe`）
一条 `cargo build --release` 产出，51 个测试全绿，CLI 与 GUI 在同一目录下结果逐条一致。
与原计划的偏差只有一处：**M6 的前端没用 React + Vite**，改成手写原生 HTML/CSS/JS
（理由见 §8）。M7 只做了裸 exe 部分：`bundle.active = false`，没有 NSIS 安装包、
没有 release workflow、没有 shell 补全。

---

## 13. 风险与已知坑

| 风险 | 影响 | 应对 |
| --- | --- | --- |
| Everything 未运行 / 精简版 / 1.5 alpha 实例差异 | 索引后端失效 | 多实例探测 + 双超时 + 强制回落；绝不因 IPC 失败终止扫描 |
| 权限不足（访问被拒） | 遍历中断 | 所有 IO 错误在候选/计量层静默降级，`tracing::debug!` 记录，不冒泡 |
| `WinSxS` 硬链接导致体积虚高 | 总数失真 | M5 用文件索引去重；README 明确 `total` 与 `sum` 两种口径 |
| 嵌套 root 重复计数 | 列表出现父子两条 | 分组阶段父子包含消解 + `ScanStats` 双口径 |
| 签名误报（普通 Node 程序含 `napi_create_buffer`） | MiniElectron 误判 | 沿用参考实现的约束：Mini flavor **只扫可执行文件**，跳过 `.so/.dll`（`src/search.rs:568-572`） |
| 分块边界漏检 | 漏报 | 64 B 重叠 + 专项单测 |
| Windows 路径归一化（`\\?\`、UNC、大小写） | 运行中状态误判 | 移植参考实现的归一化逻辑，并保留其单测作为回归基线 |
| Tauri 2 工具链（Node、WebView2、签名） | GUI 交付受阻 | GUI 放在 M6，不阻塞 CLI 主线；CI 里 GUI 单独 job、失败不阻塞 CLI 发版 |
| 全盘扫描耗时 | 用户体验 | 默认索引后端；`walk` 回落下提供进度事件与 `--root` 缩小范围 |

---

## 14. 参考索引（便于实现时对照）

| 关注点 | 文件:行（均在 `D:\Documents\GitHub\CefDetector-rs`） |
| --- | --- |
| 后端 trait 契约 | `src/search/backend.rs:36-46` |
| 索引失败回落策略 | `src/search/backend.rs:48-84` |
| 候选文件名分类 | `src/search/backend.rs:171-198` |
| 签名表与 flavor | `src/search.rs:383-410` |
| 分块扫描 + 重叠 | `src/search.rs:22-23`、`412-439` |
| magic 判断（ELF/PE/Mach-O） | `src/search.rs:441-465` |
| 目录检查与可执行文件打分 | `src/search.rs:487-615` |
| 分组去重与父子消解 | `src/search.rs:686-697`、`807-819` |
| 并行体积统计 | `src/search.rs:202-238` |
| Windows 进程枚举与路径归一化 | `src/search.rs:266-350` |
| Windows 平台排除目录 | `src/search/backend/ignore.rs:110-166` |
| Everything IPC | `src/search/backend/everything.rs`、`everything_protocol.rs` |
| CLI 输出格式（手写序列化） | `src/cli.rs:231-372` |
| C# 原版查询串与两级搜索 | `..\CefDetector\Form1.cs:146-253` |
| CI / release workflow 模板 | `.github/workflows/ci.yml`、`.github/workflows/release.yml` |

---

## 15. 立即可执行的第一步

1. 建 workspace + `cefscan-core` / `cefscan-cli` 两个 crate，`rust-toolchain.toml` 锁 1.92.0。
2. 先写 `model.rs`（`AppKind` 含 rank）与 `signature.rs`（含分块边界单测）——这是全项目正确性最敏感、也最容易测的部分。
3. 补 `backend/memory.rs` 测试替身，让 M2 结束时整条流水线已经能在无磁盘状态下跑通。
4. 再接 `walk` 后端与 CLI，形成第一个可演示的 `cefscan --root <dir>`。
