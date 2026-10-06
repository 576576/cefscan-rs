# 架构

> 面向开发者的总体设计。图形界面见 [`gui.md`](gui.md)，性能见 [`performance.md`](performance.md)，
> 测试见 [`testing.md`](testing.md)，CI 与发布见 [`ci-release.md`](ci-release.md)，
> 取舍与历史见 [`decisions.md`](decisions.md)。

## 1. 目标与非目标

### 目标

- **G1**：`cefscan` 能在 Windows 上全盘扫描，列出所有 CEF / Electron / NWJS / CefSharp /
  Edge / Chrome 应用，含路径、类型、磁盘占用、是否正在运行。
- **G2**：Everything 可用时秒级完成；不可用时自动回落到自写并行遍历。
- **G3**：输出稳定、可脚本消费（JSON / NDJSON / CSV / TOML / 表格）。
- **G4**：核心逻辑可在**不触碰真实磁盘**的前提下确定性测试。
- **G5**：图形界面提供渐进式结果流、排序、点击定位到资源管理器。

### 非目标（本期不做）

- macOS `.app` bundle 解析、Spotlight、plocate（架构留接口，实现后置）。
- 删除 / 清理 CEF 的能力（纯只读工具）。
- CEF / Electron 版本号识别（后置）。

## 2. 总体架构

```
                 ┌──────────────────────────────────────┐
                 │         cefscan-core (lib)           │
                 │                                      │
  候选源          │  ① discover   ② classify   ③ group   │
                 │  ④ size (rayon)  ⑤ running-proc      │
                 └───────────┬──────────────────────────┘
                             │ ScanEvent 流 / ScanOutcome
              ┌──────────────┴───────────────┐
              ▼                              ▼
   ┌────────────────────┐        ┌──────────────────────────┐
   │  cefscan (CLI bin) │        │  cefscanw (Tauri 2)      │
   │  clap + 输出格式化 │        │  #[tauri::command]       │
   └────────────────────┘        │  + 手写原生前端          │
                                 └──────────────────────────┘
```

核心原则：**检测逻辑只在 `cefscan-core`**，CLI 与图形界面都是它的消费者，图形界面
不调用 CLI、也不复制任何检测逻辑。候选源（遍历 / 索引）与检测逻辑完全解耦。

## 3. Workspace 布局

```
cefscan-rs/
├── Cargo.toml                 # workspace 根，[workspace.dependencies] 统一版本
├── .cargo/config.toml         # Windows 目标开 +crt-static
├── .github/workflows/{lint,build,release}.yml
├── crates/
│   ├── cefscan-core/          # 引擎库：唯一的知识沉淀处
│   │   └── src/
│   │       ├── lib.rs         # 公开 API + prelude
│   │       ├── model.rs       # AppInfo / AppKind / ScanOptions / ScanStats / 后端名常量
│   │       ├── error.rs       # thiserror：ScanError
│   │       ├── candidate.rs   # 候选文件名分类
│   │       ├── walk.rs        # 自写并行遍历（回退后端）
│   │       ├── scan.rs        # 编排：discover / scan / scan_streaming / detect_backend
│   │       ├── scan/everything.rs        # Windows Everything IPC（feature = "everything"）
│   │       ├── scan/everything_codec.rs  # IPC 编解码，平台中立（测试构建全平台编译，见 testing.md §5）
│   │       ├── signature.rs   # 签名表 + 分块扫描器（对 impl Read 工作）
│   │       ├── inspect.rs     # 目录检查：挑可执行文件、打分
│   │       ├── group.rs       # 按应用根分组、去重、父子包含消解
│   │       ├── size.rs        # 并行目录计量（rayon）
│   │       ├── process.rs     # 运行进程集合（Windows / Unix 分实现）
│   │       ├── filter.rs      # 排除规则（目录名 / 路径 / 回收站 / 平台目录）
│   │       └── naming.rs      # display_name 启发式（只被 GUI 用）
│   ├── cefscan-cli/           # 二进制名 cefscan
│   │   └── src/{main.rs, cli.rs, output.rs}
│   └── cefscan-desktop/       # Tauri 2 外壳，二进制名 cefscanw
│       ├── ui/                # 手写 HTML/CSS/JS（无构建步骤）
│       │   ├── {index.html, main.js, styles.css}
│       │   └── assets/images/background.webp   # 经典模式喜报背景（内嵌进 exe）
│       └── src-tauri/
│           ├── src/{main.rs, lib.rs, icon.rs}
│           ├── icons/{icon.ico, icon.png}
│           ├── capabilities/default.json
│           └── tauri.conf.json
├── docs/
│   ├── user-guide.md          # 用户文档
│   ├── schema.md              # 输出 schema 契约（冻结后不得随意变更）
│   └── agent/                 # 开发者文档（本目录）
├── tools/                     # 图标生成、前端校验、截图预览、冒烟测试等脚本
└── dist/                      # 本地与 CI 的产物收集目录（.gitignore 内）
```

> crate 名不带 `-rs` 后缀（仓库名带）；MSRV 由 `[workspace.package] rust-version` 声明，
> 没有 `rust-toolchain.toml`（本地与 CI 都跟最新 stable）。

**构建配置**：

- `.cargo/config.toml` 给 Windows 目标（`x86_64` 与 `aarch64`）开了
  `-C target-feature=+crt-static`，静态链接 MSVC 运行时 —— 产物只依赖 Windows 自带的核心
  DLL（KERNEL32 / user32 / ntdll …），不需要额外的 `VCRUNTIME140.dll`，也不需要 UCRT 的
  `api-ms-win-crt-*` 转发 DLL。代价是二进制略大一点，换来的是一份拷贝到任何 Win10/11 上
  都能直接跑。
- Cargo profile：`release` 走 `opt-level = 3` + `lto = "fat"` + `codegen-units = 1`
  + `panic = "abort"` + `strip`（扫描器 IO 与 CPU 都吃紧，选速度优先）；
  `dev` 保持 `opt-level = 0` 但给依赖开 `opt-level = 2`（本地 `cargo run` 的扫描速度才是
  真实体感）；`ci` 继承 `dev` 但去掉 debuginfo 并把依赖压回 `opt-level = 0`
  —— 详见 [`ci-release.md`](ci-release.md) §4。

## 4. 核心数据模型

```rust
// crates/cefscan-core/src/model.rs
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Electron, Edge, Chrome, Nwjs, CefSharp, MiniElectron, MiniBlink, Cef, Unknown,
}

impl AppKind {
    /// 全部变体，按 rank 从强到弱。`--kind` 的解析与报错列表都由它生成。
    pub const ALL: [Self; 9];
    /// 优先级，数值越大越强；用于同一目录多签名冲突时取最强者。
    pub const fn rank(self) -> u8;      // Electron 100 … Cef 60, Unknown 0
    pub const fn label(self) -> &'static str;  // 序列化值
}

pub struct AppInfo {
    pub path: PathBuf,        // 展示路径：优先可执行文件，其次应用根目录
    pub root: PathBuf,        // 用于计量与去重的根目录
    pub kind: AppKind,
    pub size: u64,            // 根目录的磁盘占用
    pub running: bool,
    pub evidence: Option<&'static str>,   // 命中的签名串，仅诊断用
}

pub enum Backend { Auto, Index, Filesystem }

impl Backend {
    pub const ALL: [Self; 3];
    pub const fn label(self) -> &'static str;          // auto / index / cefscan
    pub fn from_label(label: &str) -> Option<Self>;
}
```

两点容易踩：

- `AppKind` **故意不加 `#[non_exhaustive]`**（整个 workspace 同版本一起发，让下游 `match`
  在新增变体时编译不过，而不是静默落进 `_`）。`ScanError` 同样。
- `CefSharp` 的 serde 值**显式**写成 `cefsharp`：`rename_all = "snake_case"` 会把它拆成
  `cef_sharp`，而 `label()`、`--kind`、前端 `KIND_COLORS` 和 `docs/schema.md` 用的都是
  `cefsharp`。`model.rs` 的 `labels_match_the_serialized_form` 与 `output.rs` 的
  `every_format_agrees_on_the_kind_label` 钉住这两侧一致。

`ScanOptions` 的关键字段：

| 字段 | 说明 |
| --- | --- |
| `roots` | 遍历起点；为空则自动推导（Windows 取所有逻辑盘，其它平台取 `/`） |
| `backend` | `Auto` / `Index` / `Filesystem` |
| `exclude_dir_names` / `exclude_paths` | 排除规则 |
| `include_hidden` / `follow_symlinks` | 是否包含隐藏目录 / 跟随符号链接 |
| `walk_threads` / `scan_threads` | 遍历与扫描的并行度，`0` = 自动 |
| `index_timeout` | Everything IPC 超时（默认 1500 ms） |
| `sort` / `sort_direction` | 排序主键（`Size` / `Path` / `Kind`）与方向（`Desc` / `Asc`）；次级键恒为路径升序 |
| `detect_running` | 是否检测运行中进程 |

`ScanStats` 同时给出两种占用口径，避免"总数对不上"的困惑：

- `sum_bytes`：列表逐条求和。
- `total_bytes`：去重后的并集口径（去掉被其它根包含的目录）。

`ScanStats.backend` 是 `&'static str`，取值见 [`gui.md`](gui.md) 的"后端显示名"一节。

**为什么用 enum 而不是 String**：输出是公开契约，`"Electron"` / `"electron"` 大小写不一致
会让下游脚本崩溃；enum + `serde(rename_all)` 从类型层面杜绝。

## 5. 流水线（五阶段）

### 阶段 1 — 候选发现

候选源只负责"发现"，不做二进制检查、不做分组、不算大小。候选按文件名分成三类
（`CandidateKind`）：`Pak` / `Cef` / `Node`。

| 后端 | 平台 | 说明 |
| --- | --- | --- |
| `walk.rs` | 全平台 | 自写并行遍历，默认回退路径 |
| `scan/everything.rs` | Windows | Everything IPC，见 §6 |
| `scan/everything_codec.rs` | 全平台 | 上者的协议编解码，**不含任何 Windows API**，所以能在 Linux / macOS / Miri 下测 |
| `plocate` / `spotlight` | Linux / macOS | 后置，接口已留 |

**`Auto` 语义**：先试索引后端，**失败即回落**到遍历，并把实际使用的后端名报出来，
便于排障。

### 阶段 2 — 签名扫描（重点提速区）

沿用参考实现验证过的签名表，重构为**可并行、可复用 Finder**（预构建
`memchr::memmem::Finder`，避免每次 find 重建状态）。

| flavor | 签名（子串） | 判定 |
| --- | --- | --- |
| Standard | `third_party/electron_node`、`register_atom_browser_web_contents` | Electron |
| Standard | `url-nwjs` | Nwjs |
| Standard | `CefSharp.Internals` | CefSharp |
| Standard | `cef_string_utf8_to_utf16` | Cef |
| Mini | `napi_create_buffer` | MiniElectron |
| Mini | `miniblink` | MiniBlink |
| 文件名 | `msedge.exe` / `msedge_proxy.exe` / `chrome.exe` | Edge / Chrome（短路，不读文件） |

要点：

- 先读 magic 判断 ELF / PE（`MZ`）/ Mach-O（含 8 种 magic），不是可执行格式直接跳过，
  省掉大量无效 IO。
- 1 MiB 分块 + 64 B 重叠，避免签名跨块被截断；命中 Electron（最高 rank）立即 break。
- Mini flavor **只扫可执行文件**，跳过 `.so` / `.dll`，避免把普通 Node 程序误判成
  MiniElectron。

### 阶段 3 — 目录检查与分组

- 一个目录只做一次 `inspect`（`HashMap<PathBuf, DirInspection>` 缓存）。
- 可执行文件打分：`.exe` / `.appimage` +40、无扩展名 +30、与目录同名 +20、
  名字含 `web` / `browser` / `cef` +30，基础分 10。
- 排除噪声可执行文件：`unins*`、`*setup*`、`*report*`、`chrome-sandbox`、
  `crashpad_handler`。
- 目录内无命中时**向上一层**再试一次（Electron 常见的 `resources/` 布局）。
- 去重：以 `root` 为 key，`BTreeMap` 保证确定性；冲突时 rank 高者胜，同 rank 时
  "有可执行文件的"胜过"纯目录"。
- 父子包含消解：若 `Unknown` 的根是另一个已识别根的子路径，丢弃。

### 阶段 4 — 磁盘计量

- rayon 并行 `dir_size`，worker 数上限 `min(scan_threads, roots.len())`。
- **硬链接去重**：Linux / macOS 用 `(dev, ino)` 访问集；Windows 侧 `WinSxS` 的硬链接
  会让总量虚高（可用 `GetFileInformationByHandle` 的文件索引去重，尚未实现）。
- 两种占用口径写进 `ScanStats`（见 §4）。

### 阶段 5 — 运行进程检测

- Windows：`CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` → `Process32FirstW/NextW`
  → `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` → `QueryFullProcessImageNameW`。
- 受保护进程 `OpenProcess` 会失败，**必须静默跳过**而非 panic。
- 路径归一化：`\\?\UNC\Server\Share\x` → `\\server\share\x`，
  `\\?\C:\a/b.exe` → `c:\a\b.exe`，统一小写后比对；再 `fs::canonicalize` 兜底比对一次。

## 6. Everything IPC 后端（Windows）

这是 Windows 上唯一能秒级出结果的路径。要点：

1. Everything 必须**正在运行**且**非精简版**（Lite 无 IPC）。
2. 探测顺序要兼容 1.4 / 1.5 alpha 等不同实例（窗口类名不同）。
3. 自建隐藏窗口接收 `WM_COPYDATA` 回复，需要**两个超时**：发送超时与回复超时。
4. 查询结果一次拿全路径（`EVERYTHING_REQUEST_PATH | EVERYTHING_REQUEST_FILE_NAME`）。
5. 用**一次复合查询**减少 IPC 往返。
6. **失败必须可回落**：IPC 失败 → 遍历后端，绝不因此让整个扫描失败。

### 6.1 协议实测（已真机验证）

- **查询体**：5 个 `u32`（回复窗口句柄、回复 id、search flags、offset、max results）
  + UTF-16LE NUL 结尾的搜索串，`COPYDATASTRUCT.dwData = 2`。
- **回复体**：头部是 **7 个 `u32`（28 字节）**，第 5 个（偏移 20）是条目数；
  随后每条目 3 个 `u32`（flags、文件名偏移、路径偏移），偏移基准是**回复体起点**。
  字符串区紧跟条目数组之后，即 `28 + count * 12`。
  实测校验：`libcef` 查询返回 54 条 → 数据区从 676 = `28 + 54*12` 开始，吻合。
- 回复的 `dwData` 会带回我们发送的回复 id，不是固定常量。
- 搜索串 `file: <_100_|libcef|libnode|"Chromium Embedded Framework">` 一次覆盖
  四类候选，实测全盘返回 229 条。
- 窗口类：`EVERYTHING_TASKBAR_NOTIFICATION`（1.4）/
  `EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)`。

**踩过的两个坑**（都已修，并有回归测试）：

1. **`PCWSTR` 少写 NUL 结尾**。`to_wide()` 返回的 `Vec<u16>` 没有终止符，却被直接
   传给 `FindWindowW` 和 `WNDCLASSEXW.lpszClassName`。Win32 不报错，只会静默匹配
   不上，表现成"Everything is not running"。现在拆成 `to_wide`（构造 `OsString` 用）
   和 `to_wide_z`（给 Win32 用），并有测试钉死这个区别。
2. **索引后端没有应用 `--root` 和排除规则**。索引是全局的，而 `Filter` 原本只服务
   遍历阶段的剪枝，于是 `--backend index --root C:\Users\me` 会把整个磁盘的结果吐出来，
   还带上 `WinSxS`。现在在结果侧用 `Filter::allows_path` 再筛一遍（`allows_dir`
   保留为别名，语义上遍历筛目录、索引筛文件）。

**两个后端口径一致**：`--root C:\Users\16695` 下 index 与 cefscan 都是 21 应用 /
9.9 GiB / 29 候选；index 391 ms，cefscan 1581 ms（全盘 index 931 ms）。

> `everything` 必须是 core 的**默认 feature**。它只控制 `scan/everything.rs` 是否编译；
> 设成可选时 `cargo test` 默认不会编译那个模块的测试，等于 IPC 编解码完全没有覆盖
> —— 上面第 1 个坑就是这么漏过去的。

## 7. 依赖清单

| 用途 | crate |
| --- | --- |
| CLI 解析 | `clap`（derive） |
| 数据并行 | `rayon` |
| 子串搜索 | `memchr` |
| 序列化 | `serde` / `serde_json` / `toml` / `csv`（`serde` 在 core 里是可选 feature） |
| 错误处理 | `thiserror`（core）/ `anyhow`（CLI） |
| Windows FFI | `windows-sys`（ToolHelp / FileSystem / WindowsAndMessaging / GDI / COM …） |
| 图标（GUI，仅 Windows） | `png` / `base64` |
| 图形界面 | `tauri` 2 |
| 测试 | `tempfile`（dev-dependency） |

**feature 设计（严格 additive）**：`default = ["serde", "everything"]`，可选
`everything`、`serde`。

> 早期计划里的 `ignore`、`tracing`、`insta`、`proptest`、`assert_cmd`、`criterion`
> 都**没有**引入 —— 遍历改为自写（见 [`performance.md`](performance.md) §2），
> 测试策略相应简化（见 [`testing.md`](testing.md)）。

## 8. 风险与已知坑

| 风险 | 影响 | 应对 |
| --- | --- | --- |
| Everything 未运行 / 精简版 / 1.5 alpha 实例差异 | 索引后端失效 | 多实例探测 + 双超时 + 强制回落；绝不因 IPC 失败终止扫描 |
| 权限不足（访问被拒） | 遍历中断 | 所有 IO 错误在候选 / 计量层静默降级，不冒泡 |
| `WinSxS` 硬链接导致体积虚高 | 总数失真 | 评估用文件索引去重；`ScanStats` 明确 `total` / `sum` 两种口径 |
| 嵌套 root 重复计数 | 列表出现父子两条 | 分组阶段父子包含消解 + 双口径 |
| 签名误报（普通 Node 程序含 `napi_create_buffer`） | MiniElectron 误判 | Mini flavor 只扫可执行文件，跳过 `.so` / `.dll` |
| 分块边界漏检 | 漏报 | 64 B 重叠 + 专项单测 |
| Windows 路径归一化（`\\?\`、UNC、大小写） | 运行中状态误判 | 移植参考实现的归一化逻辑，并保留其单测作为回归基线 |
| 全盘扫描耗时 | 用户体验 | 默认索引后端；遍历回落下提供进度事件与 `--root` 缩小范围 |
