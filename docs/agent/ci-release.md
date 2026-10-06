# CI 与发布

`.github/workflows/` 下三个 workflow，职责划分照 Suwayomi-next：

| 文件 | 角色 |
| --- | --- |
| `lint.yml` | **质量门禁**：图标校验 / 前端逻辑 / rustfmt / clippy / 全量测试。**不跟 push / PR**，只由 `release.yml` 在构建之前调起（`workflow_call`），或手动 dispatch 单跑 |
| `build.yml` | **可复用构建**（只由 `workflow_call` 触发）：接收 prep 算好的版本号与**目标矩阵**，按矩阵逐条编译打包，`upload-artifact` 上传 |
| `release.yml` | **唯一入口**：推送 main → 自动 alpha；手动 dispatch → alpha / beta / release。算版本号 → 调 lint → 调 build → publish 建 Release |

## 1. `lint.yml` 的四个 job

| job | 平台 | 做什么 |
| --- | --- | --- |
| `frontend` | ubuntu | `python3 tools/check_icons.py`、`node tools/ui_harness.js` |
| `fmt` | ubuntu | `cargo fmt --all --check` |
| `clippy` | ubuntu | `cargo clippy --workspace --all-targets --locked -- -D warnings` + feature 组合矩阵 |
| `test` | windows + ubuntu | `cargo test --workspace --locked --no-fail-fast --profile ci`（Windows 上额外覆盖 `cefscanw` 的图标提取测试，那些是 `cfg(windows)` 的） |

## 2. 触发与通道

| 通道 | 触发 | 版本名 | tag | prerelease |
| --- | --- | --- | --- | --- |
| alpha | 推送 main（自动） | `0.{n/100}.{n%100}` | `v{版本}-alpha.{run_id}` | true |
| beta | 手动 dispatch | 同上 | `v{版本}-beta.{run_id}` | false |
| release | 手动 dispatch | 同上 | `v{版本}` | false |

- **版本号 = 提交数推导的三段式** `0.{n/100}.{n%100}`，其中 `n = git rev-list --count HEAD`、
  末段补零两位（58 条提交 → `0.0.58`，每满 100 条进一个 minor）。**不额外偏移** —— 参考
  实现 Suwayomi 用 `count + 3000` 把 minor 顶到 30+ 段位，那是给 Android `versionCode`
  留的，本项目不需要。
- alpha / beta 的 tag 带 `run_id`，天然唯一；release 用纯版本号，同版本重复发布时 publish
  先 `gh release delete --cleanup-tag`。
- **版本号在编译期注入二进制**：`build.yml` 传 `CEFSCAN_BUILD_VERSION`，`cli.rs` 用
  `option_env!` 读，所以 `cefscan --version` 与 Release 名一致；本地构建没有这个变量，
  回落到 Cargo.toml 的 `0.1.0`。`build.yml` 的冒烟步骤会 `grep` 这个版本号，注入失效会
  当场失败。
  > **坑**：`option_env!` 不触发重编译（cargo 不跟踪任意环境变量），本地验证回落分支要先
  > `touch cli.rs`，否则读到的还是上次注入的值。
- **纯文档 push 不出包**：`paths-ignore: ['docs/**', '*.md', '**/*.md']`。写成
  `paths-ignore` 而**不是**顶层 `paths:` —— 后者是白名单语义，会把所有代码改动的 push 一起
  挡掉，而且完全静默。`*.md` 与 `**/*.md` 两条都给：`**/` 能否匹配零级目录（根
  `README.md`）在 glob 实现之间有歧义，两条并置后两种语义下都覆盖。
- **质量门禁挂在发布链路上**：`release.yml` 的 `lint` job 与 `prep` 并行，`build` 的 `needs`
  里带上它 —— 门禁不过就直接不进入编译。推送 main 与手动 dispatch 都从 `release.yml` 进，
  所以没有绕过的路径。
- **注释块 ≤ 1 行**：决策与背景写进文档，workflow 里只留一行提示；`tools/ci_check.py` 会把关。

### 2.1 手动 dispatch 的输入

| 输入 | 类型 | 默认 | 作用 |
| --- | --- | --- | --- |
| `channel` | choice | `alpha` | 发布通道，见上表 |
| `windows_toolchain` | choice | `msvc` | `msvc` / `gnullvm` / `all`（两套都出） |
| `build_windows_x64` | boolean | `true` | 出 `windows-x86_64` |
| `build_windows_arm64` | boolean | `false` | 出 `windows-arm64` |
| `build_linux_x64` | boolean | `true` | 出 `linux-x86_64` |
| `build_linux_arm64` | boolean | `false` | 出 `linux-arm64` |
| `release_notes` | string | 空 | 手填发行说明，拼在正文最前，见 §2.3 |

- 默认值刻意与**推送 main 的自动通道**一致（x64 双平台 + msvc），手动跑一遍默认值不会多出
  计划外的产物。
- `windows_toolchain` 是**三选一**：`gnullvm` 表示"Windows 只出 gnullvm 那一份"，
  **不是**"在 msvc 之外再加一份"；要两套都出得选 `all`。要单独出 msvc 就保持默认。
- 四个架构开关**不能一个都不勾**：`prep` 会 `::error::未选择任何构建目标` 直接失败。
  工具链取值同理有白名单校验 —— 拼错的工具链会造出不存在的 rust target，白跑一轮矩阵才报错。
  这几条都由 `tools/release_scripts_check.py` 钉住（见 §3）。

### 2.2 目标矩阵怎么来的

版本号与矩阵都在 `prep` 里算**一次**，之后只经 job output 往下传，`build.yml` 不再自己推导：

```
channel + windows_toolchain + 四个开关
        │  prep：拼出目标名列表 SEL
        ▼
windows-x86_64  windows-x86_64-gnullvm  …  linux-arm64
        │  内联 python3 展开成 include 数组
        ▼
[{"target": "windows-x86_64-gnullvm", "os": "windows-latest",
  "rust_target": "x86_64-pc-windows-gnullvm", "exe": ".exe"}, …]
        │  经 prep.outputs.targets（JSON 字符串）
        ▼
build.yml：matrix.include: ${{ fromJson(inputs.targets) }}
```

- 矩阵元素的四个字段就是「一个 build job 需要的全部信息」：`target` 决定产物名与 artifact
  名，`os` 是 runner，`rust_target` 给 `cargo --target` 与 rustup，`exe` 补 Windows 的 `.exe`。
- **`target` 与 `rust_target` 分开**：Windows 的工具链这一维只体现在 `rust_target` 的后缀
  （`-msvc` / `-gnullvm`），产物名里写成 `-gnullvm` 一段 —— `x86_64-pc-windows-gnullvm`
  太长，不适合出现在 zip 名里。msvc 那份产物名保持原样（不带工具链段）。
- **空数组要显式挡**：`if: inputs.targets != '[]'`。Actions 遇到空矩阵会直接报错，而不是
  "跑 0 个 job"。`tools/ci_check.py` 断言了 `build.yml` 收得到 `targets` 这个输入。
- runner：`windows-latest` / `windows-11-arm` / `ubuntu-latest` / `ubuntu-24.04-arm`。
  Linux 的 aarch64 **不交叉编译** —— 那要交叉整套 webkit，成本高，原生 arm runner 更划算。

### 2.3 发行说明的结构

正文由 `publish` 拼出来，顺序固定：

1. **手填的 `release_notes`**（推送自动构建没有这个输入，恒为空）；
2. **架构矩阵表** —— 按**实际收到的附件**渲染，dispatch 少勾一个架构表里就没有那一行；
3. **`--generate-notes` 生成的 changelog**（结尾那段 `**Full Changelog**`）。

关键在于第 1 步与第 3 步**并存**：`gh release create` 同时带 `--notes "$BODY"` 与
`--generate-notes`，GitHub 把 body 放在自动生成内容**之前**（API 里
`generate_release_notes` 的语义就是"body 接在自动生成的说明前面"）。早期写法是二选一
（有手填说明就不带 `--generate-notes`），那会**取代**掉 changelog，现在不再这么写。

矩阵表渲染出来是这样（「下载」格是指向该 tag 附件 URL 的链接）：

| 平台 | 架构 | 工具链 | 下载 |
|:--|:--|:--|:--|
| Windows | x86_64 | MSVC | `cefscan-0.0.64-windows-x86_64.zip` |
| Windows | x86_64 | gnullvm | `cefscan-0.0.64-windows-x86_64-gnullvm.zip` |
| Linux | x86_64 | MSVC | `cefscan-0.0.64-linux-x86_64.zip` |

行序固定：平台 Windows → Linux，架构 x64 → arm64，工具链 MSVC → gnullvm。有 gnullvm
产物时表下补一句 blockquote，说明那份多带一个 `WebView2Loader.dll`（原因见 §4）。

## 3. 改 workflow 先本地校验

别靠推上去试错（一轮矩阵十几分钟，还会多出一个 alpha Release）：

```bash
"C:/Users/16695/.workbuddy-ai/binaries/python/envs/default/Scripts/python.exe" tools/ci_check.py
```

它做 YAML 可解析 + 每个 `run:` 过 `bash -n` + 注释块行数 + 触发 / 依赖结构断言（需要
pyyaml，所以用托管 venv 的解释器）。注意 **pyyaml 按 YAML 1.1 解析，`on:` 会变成布尔
`True`**，取值要写 `doc.get("on", doc.get(True))`。另外 **Git Bash 的 `/tmp` 对 Windows
python 不可见**，写临时文件要用项目内路径。

`ci_check.py` 是**静态**检查，看不见 §2.2 / §2.3 那两段的**取值** —— 矩阵是内联 `python3`
展开的、正文是一行行拼出来的，写错了 `bash -n` 一样过。所以还有第二个工具：

```bash
python tools/release_scripts_check.py
```

它把 `prep/推导版本与构建矩阵` 与 `publish/创建 Release` 两个 `run` 块抽出来、把
`${{ }}` 换成 shell 变量、给 `gh` 打桩（只打印参数），在 Git Bash 下**真跑**：断言七种
dispatch 组合的目标列表与版本号 / tag、三种错误输入会 `::error::` 退出，以及发行说明正文的
表格行、DLL 说明、「手填说明排在矩阵之前」。要 Git Bash，Windows 本地工具，不进 CI。
**它已经抓到过一个真问题**：`windows_toolchain=gnullvm` 是"只出 gnullvm 那份"，不是
"在 msvc 之外再加一份" —— 用例期望写错时它当场报了出来。

> 约定：新增断言要用**旧实现反跑**确认会失败，否则可能是恒真的（比如只断言
> `"--generate-notes" in publish` 就抓不到"退回二选一"的写法，得断言
> `gh release create` 只出现一次）。

## 4. 要点与坑

- **`build` 依赖整个 `lint.yml`**，门禁全绿才出产物。
- **Rust 用 `dtolnay/rust-toolchain@stable`，不钉版本号**。这个决定是权衡过的：测试本身
  只要 1 秒多，慢的全是编译，而编译慢不慢几乎只取决于缓存命中 —— `Swatinem/rust-cache`
  的 key 里含 rustc 版本哈希，所以 `stable` 每 6 周往前挪一次，缓存就整体失效一次，那两个
  job 要从零重编。实测同一台 runner：Sep 28 那次（1.98.1，缓存命中）测试步骤约 1 分钟；
  Oct 3 那次（1.99.0，缓存失效）测试步骤 10 分 12 秒，其中**跑测试只占 1.2 秒**。也就是说，
  那 10 分钟不是测试慢，是"每 6 周一次"的全量重编。曾经把版本钉成
  `env.RUST_TOOLCHAIN: "1.99.0"` 来躲它，后来还是回到 `@stable`：跟着最新稳定版走才能第一
  时间发现新版 rustc 的问题，而且真正把编译时间压下来的是 `--profile ci`。**要复现某次
  构建，把三处 `@stable` 换成 `@1.99.0` 这种具体版本即可**，不需要改 `env`。
- **所有 action 都用当前最新的大版本 tag**：`actions/checkout@v7`、
  `actions/upload-artifact@v7`、`actions/download-artifact@v8`、`Swatinem/rust-cache@v2`。
  `@vN` 是 GitHub 官方维护的浮动大版本 tag，补丁级安全修复会自动跟上；
  `dtolnay/rust-toolchain` 是个例外，它没有大版本 tag，只能写 `@stable` / `@master` /
  `@<版本号>`。升级前用 `gh api repos/<owner>/<repo>/releases/latest --jq .tag_name` 核一下
  真实标签，别凭印象写。
- **`test` 用 `--profile ci`**（`Cargo.toml` 里定义）：依赖不优化、不带 debuginfo。砍得最狠
  的一刀是覆盖 `[profile.dev.package."*"] opt-level = 2` —— 它本来是为了让本地 `cargo run`
  的扫描速度有参考价值，但测试根本不在乎依赖跑得快不快。本地冷 target 实测：默认 dev
  168s / `target/debug` 3.2 GB → ci profile 63s / 1.8 GB，顺带让缓存上传下载也快一截。
  本地 dev profile 不受影响。
- **`--no-fail-fast` 不能省**。cargo 默认遇到第一个失败的测试目标就停，第一次跑 Linux 时
  只看到 `cefscan-core` 的 7 个失败，doctest 和 `cefscanw` 的测试根本没跑到 —— 一次跑完
  才能拿到完整清单。
- **`check_icons.py` 放在 lint 的第一步**，纯 Python 秒级出结果。它守的是
  [`gui.md`](gui.md) §7 里那两条**只在 Unix 目标生效**的约束（`icons/icon.png` 必须存在且
  为 RGBA）。这类问题在 Windows 上根本复现不了 —— 第一次推 CI 时就是它让 Linux 编译在
  5 分钟后才炸在 `generate_context!` 里。
- **`ui_harness.js` 也放在 lint 里**，紧跟着图标校验。理由同上：纯 Node、不用
  `npm install`（runner 自带 node）、半秒跑完，却覆盖了 `cargo test` 够不着的
  `ui/main.js`。把它塞进 `test` job 只会白白多等一个 job 的排队时间。
- **Linux 每个 job 都要装 webkit 开发包**（`libwebkit2gtk-4.1-dev`、`librsvg2-dev`）。即使
  只跑测试也要装：`cefscanw` 在 workspace 里，`cargo test --workspace` 会编译它。
  **`ubuntu-22.04` 不行** —— 它只有 webkit2gtk-4.0，Tauri 2 要 4.1。
- **不需要 `libappindicator3-dev`**。tauri 在 Linux 上确实会把 `tray-icon` →
  `libappindicator` 拉进依赖图（Cargo 会下载它），但 `libappindicator-sys` 是用
  `libloading` 在**运行时 dlopen** `libayatana-appindicator3.so.1` 的，构建期不链接它，
  所以没有对应的 dev 包也编得过。
- **产物里带 README + LICENSE**，下载下来就是一个自包含目录；`build` 之后跑一次
  `cefscan --version` / `--help` 当冒烟，`cefscanw` 是 GUI 不在 CI 里启动。
- **feature 组合矩阵**（`--no-default-features` 的四种组合）单列一步，防止 `serde` /
  `everything` 悄悄退化成"其实必须开"。
- `--locked` 全用上，保证 CI 与 `Cargo.lock` 一致。
- **平台矩阵可选，默认只出 x64 双平台**。默认与推送自动通道一致；arm64 在 dispatch 里勾。
  Linux 侧不交叉编译，直接换 `ubuntu-24.04-arm` runner；Windows 侧走原生 `windows-11-arm`
  （`.cargo/config.toml` 里那份 `aarch64` 的 crt-static 配置留着，但没走交叉编译这条路）。
- **Windows 工具链 msvc / gnullvm 双份可选**（`windows_toolchain=all` 就是两套都出）。
  gnullvm 用 llvm-mingw，绕开 VS 生成工具：
  - 它的默认 linker 就是 llvm-mingw 的 `<triple>-clang`（rustc 的 target spec 里写死的），
    runner 自带的 MSVC 工具链在这个 target 下用不上。所以 `build.yml` 里那步会下载
    `llvm-mingw-20260922-ucrt-x86_64`（**版本写死**，浮动只会让产物不可复现），用 python3
    的 `zipfile` 解开（Windows runner 的 Git for Windows 没有 `unzip`），再把 `bin` 目录
    写进 `GITHUB_PATH`。路径要给 **Windows 形式**（`$(pwd -W)`），因为 `GITHUB_PATH` 的
    条目最终进的是原生进程的 PATH。
  - **gnullvm 的产物必须额外带 `WebView2Loader.dll`**：`webview2-com-sys` 按 `target_env`
    分流 —— `msvc` 静态链 `WebView2LoaderStatic`，其余动态链 `WebView2Loader.dll`，少这个
    DLL 的话 `cefscanw.exe` 起不来（`cefscan.exe` 不受影响）。DLL 从 cargo registry 的
    `webview2-com-sys-*/{x64|arm64}/` 里拷，架构跟着 `rust_target` 走。
  - 留两套的理由：`msvc` 是 Windows 上的默认 ABI、产物最省事；`gnullvm` 不依赖 VS 生成
    工具、链接更快。两套的 zip 名不同（多一段 `-gnullvm`），不会互相覆盖。

## 5. Cargo profile（release）

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true
```

> 参考实现用 `opt-level = "z"`（体积优先）。本项目是扫描器，IO 与 CPU 都吃紧，选速度优先；
> 若后续在意体积再评估。

## 6. 尚未做

- tag 触发时自动建 GitHub Release 并附产物（已由 `release.yml` 覆盖，见 §2）。
- NSIS 安装包（`bundle.active = false`，当前只要裸 exe）。
- shell 补全（`completions/` 尚未生成）。
