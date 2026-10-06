# cefscan-rs

[![Release](https://github.com/576576/cefscan-rs/actions/workflows/release.yml/badge.svg)](https://github.com/576576/cefscan-rs/actions/workflows/release.yml)

找出电脑上所有基于 Chromium 内核的应用（CEF / Electron / NWJS / CefSharp / Edge / Chrome），
给出它们的磁盘占用与是否正在运行。

产出两个**互不依赖、可独立运行**的可执行文件：

| 文件 | 类型 | 说明 |
| --- | --- | --- |
| `cefscan.exe` / `cefscan` | 控制台程序 | 纯 CLI，输出 table / json / ndjson / csv / toml |
| `cefscanw.exe` / `cefscanw` | 窗口程序 | Tauri 2 GUI，内嵌前端，无控制台 |

两者都静态链接同一个引擎 `cefscan-core`，GUI 不调用 CLI、不依赖 CLI 的存在。

> 主平台是 Windows。Linux 上也能构建运行（遍历后端 + GUI 都可用），但索引后端
> （Everything IPC）与表格里的图标列是 Windows 专属。

## 使用方法

运行 `cefscan`（CLI）或 `cefscanw`（GUI）。GUI 分三个视图 —— 初始选择页、经典模式
（喜报 + 卡片墙）、工具模式（表格）—— **共用同一份数据，切视图不重扫**。

```bash
cefscan                                   # 扫描所有盘符，表格输出
cefscan --root "C:\Users\me"              # 只扫指定目录
cefscan --format json -o result.json      # 输出到文件
cefscan --kind electron --running-only    # 只看正在运行的 Electron 应用
cefscan --min-size 500MB --sort size      # 只看 500 MiB 以上的
cefscan --backend cefscan --threads 8     # 强制遍历后端，指定线程数
```

常用参数：

| 参数 | 说明 |
| --- | --- |
| `--root <DIR>` | 遍历起点，可重复；不指定则扫描所有盘符 |
| `--backend <auto\|cefscan\|index>` | 搜索后端，默认 `auto` |
| `--threads <N>` | 线程数，`0` 表示自动（默认上限 8） |
| `-f, --format <table\|json\|ndjson\|csv\|toml>` | 输出格式 |
| `-o, --output <FILE>` | 写文件而非 stdout |
| `-k, --kind <KIND>` | 按类型过滤，可重复 |
| `--running-only` / `--min-size <SIZE>` | 过滤条件 |
| `--sort <size\|path\|kind>` / `--ascending` | 排序 |
| `--exclude-dir <NAME>` / `--exclude-path <PATH>` | 追加排除规则 |
| `-v, --verbose` / `-q, --quiet` | 诊断信息 |

退出码：`0` 成功（含 0 结果）、`1` 扫描失败、`2` 参数或配置错误。

> 完整用法见 **[`docs/user-guide.md`](docs/user-guide.md)**；输出 schema 见
> **[`docs/schema.md`](docs/schema.md)**。

## 项目特色

| 特性 | 描述 |
| --- | --- |
| 扫描性能 | 自写 rayon 并行遍历（按目录粒度并行、进入前剪枝），不依赖 `ignore`；有 Everything 索引时走 IPC，毫秒级 |
| 双后端自动切换 | 有索引服务用索引，没有则回落遍历；同一目录下两者结果逐条一致 |
| 工程可测试性 | 检测逻辑与后端解耦、纯字符串启发式（不碰文件系统）、输出确定性排序；前端有 DOM 桩 / 截图 / 端到端三层验证 |
| 无运行时依赖 | 静态 CRT，只依赖 Windows 自带 DLL；GUI 前端手写原生 HTML/CSS/JS，构建不需要 Node/npm |

## 搜索后端

| 后端 | 平台 | 依赖 | 说明 |
| --- | --- | --- | --- |
| `auto` | — | — | 先试索引，失败回落遍历（默认，也是 GUI 唯一会用的） |
| `cefscan` | 全平台 | 无 | 自写的 rayon 并行遍历 |
| `index` | Windows | Everything（非精简版） | 走 IPC 查索引，毫秒级；不可用时自动回落 |

默认排除 `node_modules`、`target`、回收站、`System Volume Information`，
以及 Windows 的 `WinSxS` / `servicing` / `Recovery`。索引是全局的，
所以 `--root` 和排除规则会在结果侧再筛一遍。

## 仓库结构

```
crates/
  cefscan-core/       扫描引擎（CLI 与 GUI 共用）
  cefscan-cli/        CLI 二进制 cefscan
  cefscan-desktop/    Tauri 2 GUI 二进制 cefscanw，ui/ 是手写原生前端
tools/                图标生成、前端校验、截图预览、冒烟测试等脚本
docs/                 user-guide.md（用户指南）+ agent/（开发者文档）+ schema.md
```

## 从源码构建

### GitHub Actions

Release 提供解压即用的平台包，每个平台一个 zip，解开就是自包含目录：

```
cefscan-{版本}-{windows|linux}-{x86_64|arm64}[-gnullvm]/
  ├─ cefscan.exe / cefscan
  ├─ cefscanw.exe / cefscanw
  ├─ README.md
  └─ LICENSE
```

推送 `main` 自动出 alpha 预发布（默认 x64 双平台）；手动触发 Release workflow 可选
`alpha` / `beta` / `release` 三个通道，并自选平台架构（x64 / arm64）与 Windows 工具链
（msvc / gnullvm / 两套都出）。带 `-gnullvm` 的包多一个 `WebView2Loader.dll`，是图形界面
要用的。每次运行的产物也挂在 Actions 页对应 run 的 Artifacts 上。

### 本地构建

只需要 Rust 工具链，**不需要 Node/npm**：GUI 前端是手写的原生 HTML/CSS/JS
（`crates/cefscan-desktop/ui/`），由 Tauri 直接内嵌进二进制。

```bash
cargo build --release
# -> target/release/cefscan.exe    纯 CLI
# -> target/release/cefscanw.exe   GUI

cargo build --release -p cefscan-cli      # 只要 cefscan.exe
cargo build --release -p cefscanw         # 只要 cefscanw.exe
```

> Linux 上编译 `cefscanw` 需要 Tauri 2 的系统依赖：
> `sudo apt-get install -y libwebkit2gtk-4.1-dev librsvg2-dev`
> （Ubuntu 22.04 不行，它只有 webkit2gtk-4.0，Tauri 2 要 4.1）。
> GUI 运行时依赖系统自带的 WebView2（Windows 10/11 默认已装）。
> `.cargo/config.toml` 开了 `+crt-static`，两个 exe 只依赖 Windows 自带的核心 DLL。
>
> 图标由 `tools/make_icon.py`（纯标准库）生成，产物已入库，正常构建无需重跑；
> `tools/check_icons.py` 会校验它，`lint.yml` 第一步就会跑。

## 开发与测试

```bash
cargo test --workspace                                 # 单测 + doctest
cargo fmt --all --check                                # 格式
cargo clippy --workspace --all-targets -- -D warnings  # 静态检查
node tools/ui_harness.js                               # 前端逻辑（DOM 桩，不需要 npm）
python tools/check_icons.py                            # 图标齐全且为 RGBA
python tools/ci_check.py                               # 改 workflow 后先静态校验（需 pyyaml）
```

CI 是三个 workflow（`lint` / `build` / `release`），约定见
**[`docs/agent/ci-release.md`](docs/agent/ci-release.md)**；
GUI 的手动冒烟测试 `python tools/gui_smoke.py` 会真的去点窗口，不进 `cargo test`。

## 关键文档

- [`docs/user-guide.md`](docs/user-guide.md) — 用户指南：CLI 用法与参数、GUI 三视图、后端、常见问题
- [`docs/agent/`](docs/agent/) — 开发者文档：架构 / GUI / 测试 / 性能 / CI 与发布 / 决策记录
- [`docs/schema.md`](docs/schema.md) — JSON / CSV / TOML 输出契约

## 许可证

MIT
