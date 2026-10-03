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
cefscan --backend cefscan --threads 8    # 强制遍历后端，指定线程数
cefscan --format ndjson | jq -r .path     # 流式消费
```

常用参数：

| 参数 | 说明 |
| --- | --- |
| `--root <DIR>` | 遍历起点，可重复；不指定则扫描所有盘符 |
| `--backend <auto\|index\|cefscan>` | 搜索后端，默认 `auto`（旧值 `filesystem` 仍可用） |
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

双击运行，或 `cefscanw.exe`。分**三个视图**，进来先是初始选择页。

**① 初始选择页**：两个模式选项在上、开始扫描按钮在下居中。选好模式点「开始扫描」即可。
这一页只有这两个选项和一个按钮，**没有目录输入框**——所以第一轮扫描是**全盘**；
想只扫某个目录，进工具模式后在工具栏里填（见下）。

**② 经典模式**：整张喜报就是画布，**不留工具条**，只有左上角两个悬浮的圆钮
（返回 / 重新扫描，**都只放图标不放文字**）。**"喜报"两个字正下方**居中一行条数
（`您的电脑里有 N 个 Chromium`，0 也照实说）。扫描出的应用以**卡片墙**呈现——
图标 + 名称 + 占用，每行放几个随窗宽自适应，**两侧留 5% 空档**（卡片墙不铺满页面，
就排在条数下方）；卡片**全透明**，只靠描边和内容成组，喜报直接透上来。
超出屏幕时**按整行**平滑向下滚动（不是滚到半行上），并且**自动跟随最新一行**；
你手动往上滚它就停下让你翻看，滚回底部又自动恢复；滚动条是隐藏的。
结果不是"啪"地一次全出来，而是**一张张缓缓浮现**。适合"我就想看看这台机器上装了
多少 Chromium 应用"。
（背景是 `assets/images/background.webp`——有损 WebP q85、122 KB，会被原样嵌进 exe，
无损版要 736 KB，而渲染后的截图差分显示两者观感无差别。）

**③ 工具模式**：深色主题的表格视图，用来细看和定位。

- **限定目录**：留空扫描所有盘符，也可以填 `C:\Users\me` 只扫一部分。
- **后端**：工具条上显示成 `自动（cefscan）` / `自动（Everything）`，括号里是**本次会
  使用的后端名**。它在你**进入工具模式时**就刷新，点一下 chip 还会再探一次——
  不必等点了"扫描"才知道。GUI 不提供后端选择：有索引服务时用索引严格优于遍历，没有时
  想选也选不上，选择项本身是伪需求。需要强制指定后端请用 CLI 的 `--backend`。
- **开始扫描**（或在输入框按回车）：扫描过程中结果**逐条流式出现**，按占用从大到小排。
- 表格列：图标 / 名称 / 类型 / 占用 / 运行 / 路径。点表头可切换排序。
  - **名称**是从路径启发式推导的可读应用名（`...\Microsoft VS Code\Code.exe` → `Microsoft VS Code`）。
  - **图标**取自系统文件关联，转成内嵌 PNG，不额外依赖任何图标文件。
  - **路径默认折叠**成「前面 3 层目录 + … + 文件名」
    （`C:\Users\16695\AppData\…\WorkBuddy.exe`），点整行展开完整路径，
    展开后可用「在资源管理器中显示」按钮定位到该目录。

扫描完成后工具条下方会给出应用数、总占用、列表合计、实际使用的后端与耗时。

**三个视图共用同一份数据**：切视图只是换个画法，**不重扫、也不清结果**，经典模式和
工具模式之间来回切都照旧看得到同一批应用。选择页那个「开始扫描」也**不总是开扫**——
手上已经有结果（或正在扫）就直接复用；一条都没有时，只有经典模式顺手开扫
（它除了左上角那个刷新圆钮没有别的扫描入口），**工具模式只切视图、不扫描**
（它有自己的工具栏，得先把目录填了再按工具栏的「开始扫描」）。两个视图的「返回」
只切视图，**不打断正在跑的扫描**。

改前端时的三步验证，都不需要起 GUI：

1. `node tools/ui_harness.js` —— 用 DOM 桩跑 `ui/main.js`，断言视图切换、后端探测时机、
   卡片墙对齐数学、切模式是否复用结果、发给后端的请求形状等 102 项行为（不需要 npm）。
2. `python tools/preview_ui.py <输出目录> [picker|classic|tool] [条数]` —— 生成一份带假
   数据的静态预览页（把 `ui/` 整个抄过去，再塞一个假的 `window.__TAURI__`），
   浏览器打开即可看效果；不给视图名就三个视图各出一张。
3. `python tools/gui_smoke.py` —— 真去点窗口的端到端（见下）。

桩验的是"跑了几次和什么顺序"，截图验的是"长什么样"，两者都跑一遍才算完整。

## 测试

```bash
cargo test --workspace              # 单测 + doctest
cargo fmt --all --check             # 格式
cargo clippy --workspace --all-targets -- -D warnings
node tools/ui_harness.js            # ui/main.js 的逻辑（102 项，不需要 npm）
python tools/check_icons.py         # 图标齐全且为 RGBA
```

上面这些（再加一组 `--no-default-features` 的 feature 组合矩阵）就是 CI 里跑的全部
检查，见 `.github/workflows/ci.yml`。

GUI 另有一个**手动**冒烟测试，会真的去点窗口，验证 Tauri command / Channel /
前端渲染这条链路（不进 `cargo test`）：

```bash
cargo build --release
./target/release/cefscanw.exe &
python tools/gui_smoke.py out.png --mode classic 6 20
python tools/gui_smoke.py out.png --mode tool --root C:/Users/me 6 20   # 额外验证限定目录重扫
CEFSCAN_SMOKE_EXPAND=1 python tools/gui_smoke.py out.png --mode tool 6  # 额外验证行展开
```

它会把窗口临时置顶（结束时会取消），免得被终端挡住。模式用键盘选（Tab / 方向键），
按钮靠"实心强调色方块"认出来，改布局一般不用改脚本。注意 `--root` 只对工具模式有效：
选择页和经典模式都没有目录输入框，给了也只会打印一句提醒。

## 搜索后端

| 后端 | 平台 | 依赖 | 说明 |
| --- | --- | --- | --- |
| `index` | Windows | Everything（非精简版） | 走 IPC 查索引，毫秒级；不可用时自动回落 |
| `cefscan` | 全平台 | 无 | 自写的 rayon 并行遍历 |
| `auto` | — | — | 先试索引，失败回落遍历（默认，也是 GUI 唯一会用的） |

默认排除 `node_modules`、`target`、回收站、`System Volume Information`，
以及 Windows 的 `WinSxS` / `servicing` / `Recovery`。

**两个后端口径一致**：同一目录下 `index` 与 `cefscan` 结果逐条相同
（实测 21 应用 / 9.9 GiB / 29 候选），只是索引后端不用遍历。
索引是全局的，所以 `--root` 和排除规则会在结果侧再筛一遍——否则
`--backend index --root C:\Users\me` 会把整个磁盘吐出来。

本机实测（16 逻辑核 / NTFS）：

| 范围 | `index` | `cefscan` |
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
