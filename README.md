# cefscan-rs

[![CI](https://github.com/576576/cefscan-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/576576/cefscan-rs/actions/workflows/ci.yml)

找出电脑上所有基于 Chromium 内核的应用（CEF / Electron / NWJS / CefSharp / Edge / Chrome），
给出它们的磁盘占用与是否正在运行。

产出两个**互不依赖、可独立运行**的可执行文件：

| 文件 | 类型 | 说明 |
| --- | --- | --- |
| `cefscan.exe` / `cefscan` | 控制台程序 | 纯 CLI，输出 table / json / ndjson / csv / toml |
| `cefscanw.exe` / `cefscanw` | 窗口程序 | Tauri 2 GUI，内嵌前端，无控制台 |

两者都静态链接同一个引擎 `cefscan-core`，GUI 不调用 CLI、不依赖 CLI 的存在。

**平台**：主平台是 Windows。Linux 上也能构建运行（遍历后端 + GUI 都可用），
但两处功能是 Windows 专属：索引后端（Everything IPC）与表格里的图标列。

## 预编译产物

CI 在 `main` 分支、PR 和 `v*` tag 上跑，并通过 `build` job 上传可下载的产物
（Actions 页面 → 对应 run → Artifacts）：

| 产物 | 内容 |
| --- | --- |
| `cefscan-windows-x86_64` | `cefscan.exe`、`cefscanw.exe`、`README.md`、`LICENSE` |
| `cefscan-linux-x86_64` | `cefscan`、`cefscanw`、`README.md`、`LICENSE` |

## 构建

只需要 Rust 工具链，**不需要 Node/npm**：GUI 的前端是手写的原生
HTML/CSS/JS（`crates/cefscan-desktop/ui/`），由 Tauri 直接内嵌进二进制。

```bash
cargo build --release
# -> target/release/cefscan.exe    纯 CLI
# -> target/release/cefscanw.exe   GUI
```

只想要其中一个：

```bash
cargo build --release -p cefscan-cli      # 只要 cefscan.exe
cargo build --release -p cefscanw         # 只要 cefscanw.exe
```

Linux 上编译 `cefscanw` 需要 Tauri 2 的系统依赖：

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev librsvg2-dev
```

（Ubuntu 22.04 不行，它只有 webkit2gtk-4.0，Tauri 2 要 4.1。）

GUI 运行时依赖系统自带的 WebView2（Windows 10/11 默认已装）。图标由
`tools/make_icon.py`（纯标准库）生成，产物已入库，正常构建无需重跑。它会出两个文件：
`icons/icon.ico`（Windows 资源）和 `icons/icon.png`（Unix 目标的窗口图标，
必须是 RGBA）。`tools/check_icons.py` 校验这两条，CI 的 lint 第一步就会跑。

`.cargo/config.toml` 里开了 `+crt-static`，MSVC 运行库静态链接进二进制。
所以两个 exe **只依赖 Windows 自带的核心 DLL**，不需要单独安装 VC++ 运行库，
拷到任何 Win10/11 上都能直接跑。`cefscanw.exe` 另外需要系统的 WebView2 Runtime。

## cefscan 用法

```bash
cefscan                                   # 扫描所有盘符，表格输出
cefscan --root "C:\Users\me"              # 只扫指定目录
cefscan --format json -o result.json      # 输出到文件
cefscan --kind electron --running-only    # 只看正在运行的 Electron 应用
cefscan --min-size 500MB --sort size      # 只看 500 MiB 以上的
cefscan --backend filesystem --threads 8  # 强制遍历后端，指定线程数
cefscan --format ndjson | jq -r .path     # 流式消费
```

常用参数：

| 参数 | 说明 |
| --- | --- |
| `--root <DIR>` | 遍历起点，可重复；不指定则扫描所有盘符 |
| `--backend <auto\|index\|filesystem>` | 搜索后端，默认 `auto` |
| `--threads <N>` | 线程数，`0` 表示自动（默认上限 8） |
| `-f, --format <table\|json\|ndjson\|csv\|toml>` | 输出格式 |
| `-o, --output <FILE>` | 写文件而非 stdout |
| `-k, --kind <KIND>` | 按类型过滤，可重复 |
| `--running-only` / `--min-size <SIZE>` | 过滤条件 |
| `--sort <size\|path\|kind>` / `--ascending` | 排序 |
| `--exclude-dir <NAME>` / `--exclude-path <PATH>` | 追加排除规则 |
| `-v, --verbose` / `-q, --quiet` | 诊断信息 |

退出码：`0` 成功（含 0 结果）、`1` 扫描失败、`2` 参数或配置错误。

输出 schema 见 [`docs/schema.md`](docs/schema.md)。

## cefscanw 用法

双击运行，或 `cefscanw.exe`。窗口顶部一行工具条：

- **限定目录**：留空扫描所有盘符，也可以填 `C:\Users\me` 只扫一部分。
- **后端**：工具条上显示成 `自动（cefscan）` / `自动（Everything）`，括号里是**本次
  实际使用的后端名**，在点下"扫描"之后立刻刷新（不是等扫完才告诉你）。GUI 不提供
  后端选择——有索引服务时用索引严格优于遍历，没有时想选也选不上，选择项本身是伪需求。
  需要强制指定后端请用 CLI 的 `--backend`。
- **开始扫描**（或在输入框按回车）：扫描过程中结果**逐条流式出现**，按占用从大到小排。
- 表格列：图标 / 名称 / 类型 / 占用 / 运行 / 路径。点表头可切换排序。
  - **名称**是从路径启发式推导的可读应用名（`...\Microsoft VS Code\Code.exe` → `Microsoft VS Code`）。
  - **图标**取自系统文件关联，转成内嵌 PNG，不额外依赖任何图标文件。
  - **路径默认折叠**成「前面 3 层目录 + … + 文件名」
    （`C:\Users\16695\AppData\…\WorkBuddy.exe`），点整行展开完整路径，
    展开后可用「在资源管理器中显示」按钮定位到该目录。

扫描完成后工具条下方会给出应用数、总占用、列表合计、实际使用的后端与耗时。

## 测试

```bash
cargo test --workspace              # 单测 + doctest
cargo fmt --all --check             # 格式
cargo clippy --workspace --all-targets -- -D warnings
```

上面三条就是 CI 里跑的全部检查（见 `.github/workflows/ci.yml`）。

GUI 另有一个**手动**冒烟测试，会真的去点窗口，验证 Tauri command / Channel /
前端渲染这条链路（不进 `cargo test`）：

```bash
cargo build --release
./target/release/cefscanw.exe &
python tools/gui_smoke.py out.png C:/Users/me 6 20
CEFSCAN_SMOKE_EXPAND=1 python tools/gui_smoke.py out.png C:/Users/me 6   # 额外验证行展开
```

它不写死控件坐标，而是从像素里认按钮和输入框，改布局一般不用改脚本。

## 搜索后端

| 后端 | 平台 | 依赖 | 说明 |
| --- | --- | --- | --- |
| `index` | Windows | Everything（非精简版） | 走 IPC 查索引，毫秒级；不可用时自动回落 |
| `filesystem` | 全平台 | 无 | 自写的 rayon 并行遍历 |
| `auto` | — | — | 先试索引，失败回落遍历（默认，也是 GUI 唯一会用的） |

默认排除 `node_modules`、`target`、回收站、`System Volume Information`，
以及 Windows 的 `WinSxS` / `servicing` / `Recovery`。

**两个后端口径一致**：同一目录下 `index` 与 `filesystem` 结果逐条相同
（实测 21 应用 / 9.9 GiB / 29 候选），只是索引后端不用遍历。
索引是全局的，所以 `--root` 和排除规则会在结果侧再筛一遍——否则
`--backend index --root C:\Users\me` 会把整个磁盘吐出来。

本机实测（16 逻辑核 / NTFS）：

| 范围 | `index` | `filesystem` |
| --- | --- | --- |
| `C:\Users\16695`（4.4 万目录） | 391 ms | 1581 ms |
| 全盘 | 931 ms | — |

## 设计要点

- **自写遍历而非 `ignore`**：cefscan 不需要 gitignore 规则，也不需要为每条路径做
  一次 stat。自写实现按目录粒度并行、在进入目录前剪枝，实测与 `ignore` 同量级且更可控。
  选型对比数据见[实施计划](docs/init-cefscan-rs.md#92-fsindex-评估结论不适合-cefscan已实测否决)。
- **签名优先级**：同一目录命中多条签名时取最强者，
  Electron 100 > Edge/Chrome 95 > NWJS 90 > CefSharp 80 > MiniElectron 75 > MiniBlink 70 > CEF 60。
- **确定性输出**：结果必须排序后输出，遍历顺序不确定不影响结果。

## 协议

MIT
