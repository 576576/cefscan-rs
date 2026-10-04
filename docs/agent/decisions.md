# 决策记录与参考

## 1. 参考实现盘点

参考对象：`D:\Documents\GitHub\CefDetector`（C# / WinForms 原版）与
`D:\Documents\GitHub\CefDetector-rs`（Rust / egui 重写版）。**只参考架构与经验，不 fork、
不复制代码。**

### 1.1 两个参考仓库对比

| 维度 | CefDetector (C#) | CefDetector-rs (Rust) |
| --- | --- | --- |
| 规模 | `Form1.cs` 254 行，单文件 | `src/**` 约 10.4k 行，15 个模块 |
| UI | WinForms（Button + Panel + 背景音乐） | egui + glutin + winit 自绘（`gui.rs` 2031 行） |
| 搜索 | 仅 Everything SDK（`Everything64.dll` P/Invoke） | trait 抽象 4 后端：ignore / plocate / Everything IPC / Spotlight |
| 类型识别 | 4 条签名 | 同上 + Mini 分支，并带优先级 rank |
| 配置 | 无 | `config.rs` **3045 行**（含 GUI 像素级配置 + 带行列号的 TOML 诊断） |
| 输出 | 只有 GUI | CLI 支持 TOML / JSON / CSV（手写序列化器，无 serde_json） |
| 测试 | 无 | 各模块 `#[cfg(test)]`，fixture 用 tempdir 造假 Mach-O / PE / `.app` |

### 1.2 值得继承的（已被实践验证的确定性知识）

1. **三段式流水线**：候选发现 → 二进制签名扫描 → 按目录分组计量。后端只负责"发现候选"，
   检测逻辑与后端完全解耦。
2. **候选文件名分类表**：
   - `*_100_*.pak` → `Pak`（对应 `chrome_100_percent.pak`）
   - `libcef.so` / `libcef.so.*` / `libcef.dll` / `libcef.dylib` /
     `Chromium Embedded Framework` / `Electron Framework` → `Cef`
   - `libnode.so[.*]` / `libnode.dll` / `libnode.dylib` → `Node`（MiniElectron / MiniBlink
     线索）
3. **签名优先级 rank**：Electron 100 > Edge/Chrome 95 > NWJS 90 > CefSharp 80 >
   MiniElectron 75 > MiniBlink 70 > CEF 60。命中最高 rank 时（Electron）可提前结束该文件
   扫描。
4. **分块扫描 + 重叠**：1 MiB 块 + 64 B 重叠，避免签名跨块被截断。
5. **Windows 进程路径必须归一化**：`\\?\` 前缀剥离、`\\?\UNC\` 还原为 `\\`、斜杠统一、
   整体小写比较。这是最容易出 bug 的地方。
6. **平台排除目录**：Windows 侧排除 `WinSxS` / `servicing` / `Recovery` /
   `System Volume Information`，且大小写不敏感但必须是**精确目录边界**（`WinSxSBackup`
   不能被误伤）。

### 1.3 明确不继承的

- **不继承 3045 行的配置系统**。cefscan 只做扫描，配置面控制在 20 个键以内。
- **不继承 egui 自绘 GUI**。改用 Tauri 2 + Web 前端，把渲染复杂度移出 Rust。
- **不继承手写序列化器**。直接用 `serde_json` / `csv` / `toml`，换取正确性与 schema 稳定。
- **不继承 `AppInfo.app_type: String`**。改用 `#[non_exhaustive] enum AppKind` +
  `serde(rename_all = "snake_case")`，让输出 schema 可被机器安全消费。
- **不继承串行签名扫描**。参考实现是逐文件串行的，这是本项目最主要的提速空间。

## 2. 里程碑与进度

| 里程碑 | 内容 | 验收标准 |
| --- | --- | --- |
| **M0 骨架** | workspace、crate 划分、CI（fmt / clippy / test）、`.gitignore`、README、LICENSE | `cargo clippy -D warnings` 干净；CI 三平台绿 |
| **M1 候选发现** | `model.rs`、`error.rs`、`candidate.rs`、`walk.rs`、`filter.rs` | 分类测试全绿；遍历后端能在指定 root 找出 fixture 里的 `libcef.dll` |
| **M2 签名扫描** | `signature.rs`、`inspect.rs`；签名表 + rank + 分块重叠 + magic 过滤 | 分块边界、跨块强签名、8 种 Mach-O magic 用例全绿 |
| **M3 分组计量 + CLI** | `group.rs`、`size.rs`、`process.rs`、`scan.rs`、CLI（table/json/csv/toml） | 输出符合 `docs/schema.md`；运行进程高亮在 Windows 实测有效 |
| **M4 Everything 后端** | `scan/everything.rs`、IPC 协议、超时、自动回落 | 装了 Everything 的机器上秒级出结果；未装 / 精简版时自动回落且不失败 |
| **M5 性能专项** | 并行签名扫描、预构建 Finder、热路径去分配 | 与 M4 基线对比有可量化提升并写入 [`performance.md`](performance.md) |
| **M6 Tauri 2 GUI** | `cefscan-desktop`、手写前端、流式 Channel、资源管理器定位 | 冷启动 < 1.5 s；扫描过程中列表渐进增长不卡 UI；点击能正确定位 |
| **M7 发布工程** | Release workflow、版本号推导与注入、`docs/schema.md` 冻结 | 三个通道都能产出可下载产物；版本号由提交数推导并注入二进制 |

**后置**：Linux `plocate` 后端、macOS Spotlight + `.app` bundle、CEF / Electron 版本号识别。

**当前进度**：M0–M6 已落地并真机验证 —— 两个 exe（`cefscan.exe` / `cefscanw.exe`）由一条
`cargo build --release` 产出，全量 `cargo test` 与 124 项前端断言全绿，CLI 与 GUI 在同一
目录下结果逐条一致。与原计划的偏差有两处：**M6 的前端没用 React + Vite**，改成手写原生
HTML/CSS/JS（理由见 [`gui.md`](gui.md) §1）；**M7 的发布链路已经落地**。还没做的是打包形态：
没有 NSIS 安装包、没有 shell 补全。

## 3. 参考索引（便于实现时对照）

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

## 4. 文档沿革

本目录（`docs/agent/`）由早期那份单文件"实施计划"（`docs/init-cefscan-rs.md`）拆分而来，
按主题拆成架构 / GUI / 性能 / 测试 / CI 与发布 / 决策六份文档（索引见
[`README.md`](README.md)）。当初的单文件已随重构删除，其中的设计取舍与踩坑记录都保留在
对应主题的文件里。
