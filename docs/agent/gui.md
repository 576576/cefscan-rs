# 图形界面（cefscanw）

> 用户视角的操作说明见 [`../user-guide.md`](../user-guide.md) 第 3 节。本文是开发视角。

## 1. 前端形态与通信

> **实施偏差（已落地）**：原计划用 React 19 + TS + Vite，实际改为**手写原生 HTML/CSS/JS
> + `withGlobalTauri`**，彻底去掉 Node 工具链。理由：图形界面只有一个选择页、一张卡片墙、
> 一张表格，引入打包器带来的收益抵不过成本 —— 构建要装几百 MB 的 `node_modules`，
> 前端产物还要跟 Rust 产物分别管理，而本项目的硬约束是"一条 `cargo build --release`
> 出两个 exe"。列表规模用「只渲染前 500 条 + rAF 合并重绘」解决，不需要虚拟滚动。

- **前端**：`crates/cefscan-desktop/ui/`，三个文件（`index.html` / `main.js` / `styles.css`），
  无构建步骤，由 Tauri 直接内嵌。通过 `app.withGlobalTauri = true` 拿到
  `window.__TAURI__.core.{invoke, Channel}`。
- **通信**：
  - `#[tauri::command] async fn scan_apps(channel, request)` → 在
    `tauri::async_runtime::spawn_blocking` 里跑 core（CPU 密集，绝不能堵住 async 运行时）。
  - 结果通过 Tauri 2 `Channel` **流式推送**，而不是等全部扫完。
  - `scan_streaming` 的回调在**计量阶段**边算边发（`sizes_parallel_each` 每算完一个
    根目录就回调一次）。应用体积差异很大（小的几十 MB、大的几个 GB），逐个发射能让
    用户立刻看到结果。代价是发射顺序不确定，所以 `scan_streaming` **不排序**，排序由
    `scan()` / 前端各自负责（`sort_apps` 是共用实现）。
  - 事件类型：`ScanEvent::{Started{backend}, Item(AppRow), Done{...}, Error{message}}`，
    serde 用 `tag = "type"` 打标签。`Started` 带后端名，且**先于任何 `Item`**。
- **`ScanEvent` 的线上格式是前端的唯一契约**，由 `src-tauri/src/lib.rs` 的
  `scan_events_keep_their_wire_format` 钉死（Rust↔JS 没有类型检查，改名 / 漏 camelCase
  只会静默失灵）。`Started` 是带 `backend` 的 variant，`Item` 是 newtype variant，
  内部标签模式下被摊平成"AppRow + type"。`cefscanw` 为此有 `serde_json` dev-dependency。
- **窗口标题就叫 `cefscanw`**，不挂副标题。工具条左上角那个 `.brand` 也是同一个名字。
- **边界**：图形界面不复制任何检测逻辑，只做 `cefscan-core` 的消费者；core 不依赖 Tauri。
- **构建**：`bundle.active = false`，只要裸 exe 不要安装包；`main.rs` 上
  `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` 去掉控制台。
  运行期依赖系统自带 WebView2（Win10/11 默认已装）。

## 2. 后端显示名与状态标签（chip）

`ScanStats.backend` 是 `&'static str`，值只有两种：

| 后端 | 显示名 | 来源 |
| --- | --- | --- |
| 文件系统遍历 | `cefscan` | `model::FILESYSTEM_BACKEND` 常量 |
| 索引 | **实际服务名**（如 `Everything`） | `scan/everything::SERVICE_NAME` 常量 |

两个常量都放在 `model.rs`，它是**唯一取值来源**：`ScanStats::backend`、
`ScanNotice::backend`、`scan::detect_backend()` 全读它，改名只需动这一处。
索引后端返回服务名而不是笼统的 `index`，是为了以后接 plocate / Spotlight 时显示名能
自动跟着变，前端和 CLI 都不用改。

CLI 的 `--backend` 取值跟着一起叫 `cefscan`（`auto|cefscan|index`），不再叫 `filesystem`：
用户看到的"后端"就是"谁去找的"，遍历后端就是 cefscan 自己。取值**次序**由 enum 变体
声明次序决定，一并决定 `--help` 里的展示次序，所以 `Cefscan` 排在 `Index` 前面。
`filesystem` 这个旧值**不留兼容 alias**，旧写法会直接报参数错误。core 里的枚举仍叫
`Backend::Filesystem` —— 它描述的是机制（文件系统遍历），对外名字由常量决定。

**图形界面不提供后端选择**，只有"自动"。理由：有索引服务时用索引严格优于遍历
（毫秒 vs 秒，结果逐条一致），没有时想用也用不上，用户没有决策所需的上下文，却要为
选错负责。但"自动"两个字没有信息量，所以**括号里必须实时显示它选了谁**：

```
自动（待检测）  →  自动（cefscan） / 自动（Everything）
```

关键是"实时"要真的实时。后端名原本只在 `Done` 里回传，用户得等整轮扫描结束才知道
后端是谁 —— 那时候知道也没用了。所以 `scan_streaming` 多了一个 `on_notice` 回调
（`FnOnce(ScanNotice)`，语义上只该发生一次，也不需要 `Send`：通知在调用者线程上同步
发出，不进任何工作线程池），在 `discover()` 返回的那一刻就调用：

```rust
let (candidates, backend_name, dirs_scanned) = discover(options)?;
on_notice(ScanNotice { backend: backend_name });
```

配套的两处顺序调整：

- **`running_processes()` 从 `discover()` 之前挪到之后**。它跟选后端毫无关系，但枚举
  进程要几十毫秒；挪到后面能让这条通知更早到达。
- **`scan()` 传 `|_| {}`**：CLI 不需要这条通知（它的 `--verbose` 已在末尾打印后端）。

**但"开扫之后"还不够早**。用户进工具模式第一眼看到的就是那个 chip，那时扫描还没开始；
如果 chip 一直写着"待检测"，"自动"这个选项就不可信 —— 用户没法在按下去之前知道它会
选谁。所以 chip 的刷新时机是 **进入工具模式时 + 点 chip 时**，都不等点"开始扫描"。

为此 core 多了 `scan::detect_backend(&ScanOptions) -> &'static str`：

```rust
pub fn detect_backend(options: &ScanOptions) -> &'static str {
    match options.backend {
        Backend::Filesystem => FILESYSTEM_BACKEND,
        // "自动"和"只用索引"的差别只在**失败之后**：前者回落遍历，后者报错。
        // 挑后端那一刻两者看到的可用性判断是同一个，所以名字也一样。
        Backend::Auto | Backend::Index => index_service_name().unwrap_or(FILESYSTEM_BACKEND),
    }
}
```

它**必须便宜**：只做 `FindWindowW` 窗口探测（`everything::is_service_available()`），
**不发查询、不等 `index_timeout`**（默认 1.5 s）—— chip 每次进视图 / 被点都要刷，
卡 1.5 s 不能接受。探测和真查询共用同一个 `find_service_window()`，保证"报得出来的
服务"和"问得到的服务"永远是同一个。代价是它只承诺"会选谁"，不保证那次查询一定成功
（窗口在但 Everything 卡死时照样报 true）；真开扫时后端仍可能超时并（在 `Auto` 下）
回落到遍历。

`detect_backend` 的选择策略**必须与 `discover()` 一致**，否则 chip 上显示的和结果里
报的会是两个东西。这条由 `probe_and_scan_report_the_same_backend` 钉死；另有一条
`probe_never_waits_on_the_index_timeout`（把 `index_timeout` 设成 30 s，断言 1 s 内
返回）守住"便宜"这个性能契约。

Tauri 侧是 `detect_backend` 命令，返回 `BackendProbe { backend }` —— **和
`ScanEvent::Started` 同形状**，前端两处读到的东西长一样。它**吃整个 `ScanRequest`**
而不是单个 backend 字符串：探的就是"你即将用的那份参数会选谁"，前端把同一份 request
先交给探测、再交给 `scan_apps`，chip 上写的和结果里报的不可能对不上。

前端还多了一道**写入代次闸**（`backendEpoch`）：`setBackendLabel()` 每次自增，
`refreshBackend()` 在 `await` 回来之后对不上号就丢弃自己的结果。没有它会出现一个真实的
竞态 —— 一次在途的探测会在扫描已经失败之后把"未确定"又盖回成一个后端名，用户看到的是
"失败了，但后端是 cefscan"，自相矛盾。这个 bug 是 `ui_harness.js` 抓出来的。

文案上刻意避开"遍历"/"索引"这类内部叫法，直接显示具体后端名（`cefscan` /
`Everything`）—— 用户看到的是"谁去干的活"，而不是"用了哪种算法"。`ScanEvent::Started`
的 payload 里只有后端名，前缀"自动"由前端拼（`backendLabel()`），这样以后真加了后端
选择，前端改一处即可。

回归测试在 `scan.rs`：`notice_reports_the_backend_before_any_result` 断言通知**排在
第一条结果之前**（`log.first() == "notice:cefscan"`），这是"实时"这个词的可执行定义。
另外两条（`auto_backend_falls_back_to_cefscan_and_says_so`、
`index_backend_without_a_service_is_an_error`）带
`#[cfg(not(all(feature = "everything", target_os = "windows")))]` 门控 —— Windows 上装了
Everything 的机器行为不确定，只有"没有索引服务可用"的平台才能断言。

## 3. 应用名（`display_name`）

`cefscan_core::display_name(path)`（`crates/cefscan-core/src/naming.rs`）。扫描结果里的
path 是"最能代表这个应用的那个文件或目录"，直接当名字没法看
（`...\Microsoft VS Code\Code.exe` → "Code"，
`...\Edge\Application\154.0.4258.37\msedge.exe` → "msedge"）。

启发式是**纯字符串**的（不碰文件系统，因此好测）：从所在目录往上走最多 6 层，跳过三类
"不是应用名"的目录：

1. **版本号目录**（`is_version_like`）：`154.0.4258.37`、`app-3.6.6`、`Workstation-17.0.0`。
   判据是"剥掉前导字母和分隔符后剩下纯数字 + 点 / 横线 / 下划线"，这样 `BeamNG.drive`
   （剥完是空）和 `360se6`（含字母）不会被误判成版本号。顺带 `x86_64` 会被它认成版本号
   （`86_64` 全数字 + 下划线），这正是想要的。
2. **通用目录名** `GENERIC_SEGMENTS`（整段精确匹配，忽略大小写）：
   `Application`、`Bin64`、`runtime`、`32bit`、`32-bit`、`64bit`、`64-bit`、`bin32` …
3. **通用后缀目录** `GENERIC_SUFFIXES`（当前只有 `_data`）：`<前缀>_Data` 家族 ——
   Unity 的 `BH3_Data`、ASP.NET 的 `App_Data`，以及本机一大片 `Cache_Data` /
   `crash_data` / `module_data`。穷举不划算，按后缀匹配更稳。

取第一个有意义的段；撞到用户 / 系统目录（`Programs`、`LocalAppData`、`steamapps` …）
就停，退回文件名。

- **`display_name` 只被图形界面用**（`src-tauri/src/lib.rs` 的 `AppRow.name`），CLI 的
  table / JSON 都不输出 name 字段 —— 所以它没法用 CLI 端到端验证，只能靠单测。
- **改启发式先看 `naming.rs` 里那张 13 条真实路径的期望值表**（含 `BH3_Data`、
  VMware 的 `64bit`）。
- 裸 `Data` 目录**暂不排除**（只排 `_data` 后缀）；若以后要连 `Data` 一起排，往
  `GENERIC_SEGMENTS` 加 `"data"` 即可。

## 4. 图标列

`crates/cefscan-desktop/src-tauri/src/icon.rs`。链路：
`SHGetFileInfoW(SHGFI_ICON|SHGFI_LARGEICON)` → `HICON` → `GetIconInfo` 拆出彩色位图与
掩码 → `GetDIBits` 取 32bpp 自顶向下 BGRA → 补 alpha → PNG → `data:image/png;base64,…`。

三个实现选择：

- **用 `GetDIBits` 而不是 `DrawIconEx` 画进 DIB**：前者拿到的是位图原始像素，行为确定；
  后者是否保留 32bpp 图标的 alpha 通道取决于具体 GDI 实现。代价是老式图标要自己补
  alpha —— 判据是"彩色位图 alpha 全为 0"，这时改用掩码位图（白 = 透明、黑 = 不透明）。
  纯单色图标（`hbmColor` 为空）直接放弃，返回 `None`，前端留空格。
- **结果按路径缓存**（`OnceLock<Mutex<HashMap>>`）：同一个 exe 在列表里可能重复出现，
  而且每次 `SHGetFileInfoW` 都要碰一次 shell。失败也缓存，免得反复问。
  `SHGetFileInfoW` 要求线程先 `CoInitializeEx`，用 thread-local 挡一下重复初始化。
- **取图标这一段全局串行**（`imp::capture` 里一把 `Mutex<()>`）：`SHGetFileInfoW`
  **不能并发调用**。4 线程同时对同一个 exe 调用，240 次里有 3 次直接返回 0（拿不到
  `HICON`）。失败点在 shell 调用本身 —— 同一轮实测里 `GetIconInfo` / `GetDIBits` 都是
  0 次失败，所以不是我们销毁句柄的问题。**这个并发在真实使用中一定会发生**：图形界面的
  图标提取跑在 rayon 工作线程上（`sizes_parallel_each` 的并行回调里），不加锁的表现是
  界面上偶发少一个图标。加锁后同样并发跑 0 失败；PNG 编码在锁外做。代价可以忽略：结果
  本来就按路径缓存，一次扫描最多几十个不同的 exe。

`icon.rs` 自带四条单测（拿测试进程自己的 exe 当样本）：PNG 签名与正方形尺寸、**解出来
必须有非透明像素**（防 alpha 补错导致整列空白格）、不存在的路径返回 `None`、**4 线程
并发提取同一个 exe 不许失败**（上面那个并发 bug 的回归测试；它直接打 `imp::extract`
而不是 `data_url`，否则会被结果缓存挡住、走不到 shell 调用）。

## 5. 三视图

界面上线时是"一条工具条 + 一张表格 + 一个经典模式勾选框"。用户要求改成**三个视图**，
理由是勾选框把两件不同的事（外观 / 信息密度）压成了一个开关：

```
① 初始选择页  ──►  ② 经典模式（卡片墙）  ──►  返回
              └──►  ③ 工具模式（表格）    ──►  返回
```

- **① 初始选择页**（`#picker`）：两个模式选项在上、**「进入」按钮**在下居中（倒三角排布）。
  **只有两个选项 + 一个按钮，没有目录输入框**（用户明确要求）。所以**第一轮扫描永远是
  全盘** —— `#root-input` 在隐藏的工具视图里、值是空串。想限定目录得进工具模式再输一次。
  **别"顺手"往选择页加输入框**。
  按钮文案是**「进入」而不是「开始扫描」**：它只负责把人送进选中的视图，进去之后扫不扫
  得看情况（有结果就复用、工具模式只切视图）。写"开始扫描"就是在骗人。正因为它是
  **导航**而不是动作，`setScanning` 里它**既不改文案也不禁用** —— 扫描中禁用的话，
  从视图里点「返回」就再也进不去了。工具模式那个按钮才是动作，照旧在"开始扫描 /
  扫描中…"之间切、扫描时禁用。
- **② 经典模式**（`#classic-view`）：**整张喜报就是画布，不留工具条**（用户明确要求），
  只有左上角两个悬浮的**图标圆钮**（返回 / 重新扫描）和"喜报"二字正下方的一行条数。
  结果画成**卡片墙**，见 §5.2。
- **③ 工具模式**（`#tool-view`）：原来的工具条 + 汇总栏 + 表格，多一个返回。

**三个视图共用一份数据源 `rows`**，各自只是它的一种画法（`render()` 按 `view` 分发到
`renderTable()` / `renderCards()`）。**切视图只是换个画法：不重扫、也不清结果**，所以在
经典模式和工具模式之间来回切都看得到同一批应用。**两个视图的「返回」只切视图、不打断
正在跑的扫描**。

**顺序上两个视图各管各的**。`rows` 的本源顺序是**扫描结果的到达顺序**，经典模式的卡片墙
直接按它画 —— 每张新卡片都追加在末尾。工具模式的排序（点表头）只在 `renderTable()` 里
对**副本**做（`sortedRows()`），**不原地排 `rows`**。所以在工具页点了表头排序，切回经典页
时卡片的相对位置原样不动（用户明确要求"保持卡片弹出后的相对位置不变"）。反过来，若原地
排 `rows`，两个视图就会互相干扰。harness 用"到达顺序故意打乱"的数据钉住这一点。

**选择页那个「进入」不总是开扫**。病根是原来那个监听无条件 `void runScan()`，每点一次
就把 `rows` 清掉重扫。改成先切视图、再看条件：

```js
startButton.addEventListener('click', () => {
  const chosen = document.querySelector('input[name="mode"]:checked');
  const next = chosen && chosen.value === 'tool' ? 'tool' : 'classic';
  showView(next);
  if (rows.length > 0 || scanning) return;   // 手上有结果（或正在扫）→ 只切视图
  // 一条都没有时，只有经典模式顺手开扫：它除了那个刷新圆钮没有别的扫描入口。
  // 工具模式不自动扫——它有自己的工具栏，得先让人把目录填了再按"开始扫描"。
  if (next === 'classic') void runScan();
});
```

经典模式左上角那个**刷新圆钮**（`#classic-refresh`）则是无条件 `runScan()` 重扫一次。

`view === null`（选择页）时 `render()` 什么都不画。这种状态下到达的结果由 `pushRow`
直接标成 `painted` 进 `rows`，用户再进某个视图时是一次画完、不补入场动画 —— 他本来就
没在看，没必要让两百张卡片一起演一遍入场。

**只有经典模式用喜报皮肤，选择页和工具模式都是深色**。所以 `<html>` 开局**不带任何主题
类**（深色就是 `:root` 的默认值），`showView()` 里
`document.documentElement.classList.toggle('classic', next === 'classic')` 才加上。
好处是顺带解决了"脚本跑起来之前闪一下深色"的问题。

### 5.1 经典模式顶部：一条不滚动的带

- **顶部区域从滚动区里挪出来**（`.classic-top { flex: none }`）：两个图标胶囊 + 条数都在
  这里，**只有下面的 `#cards` 滚**。这样"卡片墙排在条数下方"是**恒定**的，不会滚着滚着
  把条数顶走。副作用是 `#cards` 的 `padding-top` 变成 **0** —— 原来那 56px 是给悬浮 HUD
  让位的，现在 HUD 不在滚动区里了。
- **条数用百分比定位**（`padding: 15% 24px 12px`），**不能用固定 px**。背景是
  `center top` + `cover`，缩放由窗口**宽度**决定，所以"喜报"两字下沿在窗口里的高度与
  宽度成正比（原图 1000×749、下沿 y≈145 → 145/1000 ≈ 14.5%）。写死 px 的话窗口一拉宽，
  "喜报"就跑到字下面去了。
- **条数只有一种文案**：`您的电脑里有 N 个 Chromium`，0 也照实说 —— 所以 `renderCards`
  里**没有空态分支**。它挂在 `#classic-count` 上，**不是** `.status-text`，所以不在
  `setScanning` / `setStatus` 那套"按 class 一把改"的循环里，由 `renderCards` 每次重绘
  时刷。
- **两个胶囊只放图标**：内联 `<svg>`（返回箭头 / 刷新箭头），`stroke="currentColor"`，
  等宽等高 34×34 的圆钮。**不能走 `setScanning` 那个 `textContent = …` 循环** ——
  会把图标本身抹掉，所以刷新胶囊单独设 `disabled`。

### 5.2 卡片墙

#### 卡片：全透明、收紧内边距、两侧留白、隐藏滚动条

- **应用名被裁是"定高装不下"的必然结果**。`--card-h` 是写死的（"按整行滚动"要靠它），
  内容装不下时 `overflow: hidden` 就切掉名称的**下伸笔画**（g/p/y）。
  现在是 `95 - 8×2 = 79` 可用，需要 `32(图标) + 4 + 18(名称) + 4 + 15(占用) = 73`。
  所以 `padding: 8px 10px`、`gap: 4px`，并且 `.card-name` 的**字号和行高都写死**
  （`13px` / `1.35`）—— 继承 body 的 `14px × 1.5 = 21px` 就装不下了。
  **这套尺寸跟 `--card-h` 是绑死的**：单独把字号调大一档，切字的问题就会回来。
- **卡片全透明**（`--card-bg: transparent`）：喜报直接透上来，卡片只靠 1px 描边和内容
  成组。这个变量放 `:root`（两个主题都用它），不在 `html.classic` 里覆盖。
- **两侧留白**：`#cards` 的 `padding` 从 `56px 16px 18px` 改成 `0 5% 18px` —— 上方归 0
  （顶部区域挪走了），左右各 5% 空档让卡片墙不铺满页面。
- **隐藏滚动条**：`scrollbar-width: none` + `.cards::-webkit-scrollbar { display: none }`。

#### 卡片密度：三个尺寸是一组，要动一起动

密度 = 一屏放得下几张，跟单个卡片的**占位面积**成反比，所以要把 `--card-min` /
`--card-h` / `--card-gap` **一起**乘 `1/√1.5 ≈ 0.8165`（横向乘一次、纵向乘一次，
合起来正好 1.5 倍）：

| | 原值 | 现在 |
| --- | ---: | ---: |
| `--card-min` | 168px | **137px** |
| `--card-h` | 116px | **95px** |
| `--card-gap` | 14px | **11px** |

- **只改一个是不行的**。只压 `--card-min` 会让卡片变成又窄又高的怪比例；只压 `--card-h`
  则会重新切掉应用名的下伸笔画。所以这一组要当成**一个数**来调（密度跟
  `(card-min+gap) × (card-h+gap)` 成反比）。
- 内容尺寸必须**同比**跟着收：图标槽 40 → 32（`styles.css` 的 `.card-icon` 和
  `main.js` 的 `cardIconHtml` **两处**，改一处卡片高度就会参差不齐）、名称字号 14 → 13、
  占用字号 12 → 11。
- 实测：1280 宽的窗口里每行从 4 张变成 **7 张**，一屏可见张数 21 → 31（≈1.48 倍）。

#### 出场与滚动的速度上限（"仪式感"）

- **单拍上限**（`REVEAL_MAX_PER_TICK = 3`）：不管积压多少，一拍最多搬 3 张。速度因此是
  恒定的 3 张 / 150ms = **20 张/秒**。
- **预算留着**（`REVEAL_STEP_MS = 150` / `REVEAL_BUDGET_MS = 12000`）：`step` 取两者的
  交集 —— 按预算算要搬几张，但绝不超过上限。66 条时预算算出来是 1 张/拍（约 10 秒放完），
  500 条时是 7 张/拍、被上限砍到 3 张（约 25 秒）。**没有上限就只有"快"，没有预算就只有
  "慢到没法用"**，两个都要。
- **滚动改用自己按帧推的动画**（`scrollCardsTo` / `scrollFrame`），速度上限是一个明确的
  常量 `SCROLL_MAX_PX_PER_SEC = 340`（px/秒，≈3.2 行/秒 @106px 行距）。索引后端会在几百
  毫秒里吐几十条，`desired` 一下能跳到十几行外；交给按帧推的动画，位置由我们写、每帧最多
  挪 `SCROLL_MAX_PX_PER_SEC × dt`（帧长夹在 64ms 内）。收尾再减速
  （`SCROLL_EASE_PX = 90`，下限 `SCROLL_MIN_PX_PER_SEC = 70`），免得贴到目标前"咔"一下
  停住。对齐判据不受影响：目标仍是 `padTop + k*行距`。
- **不能用 `scrollTo({ behavior: 'smooth' })`**。两个原因：① 它的速度由浏览器定、没法调；
  ② 它是**异步**的，`scrollTop` 不会立刻变 —— 想按位置限速根本配合不了：位置还没动，
  下一帧算出来的目标又是同一个，会被"目标没变就跳过"挡掉，**墙干脆一动不动（实测真的
  卡在顶部）**。自己按帧推就没这个问题。
- **`lastScrollTop` 只能由 scroll 监听写**。自驱动画写的是 `scrollTop` 本身；它要是也去写
  `lastScrollTop`，scroll 事件里的旧值和动画写的新值就会交错，每一帧向下滚都被误判成
  "用户往上滚"，跟随当场永久停摆。harness 里有一条断言盯着"动画不碰 `lastScrollTop`"。
- harness 的**场景 7.1** 用真实落差采样验证限速：补 60 张卡制造 ~640px 的落差，逐帧记录
  `scrollTop`，断言"目标跳了 >500px、确实是一帧帧挪过去的（>50 帧）、单帧最大位移不超
  上限"。不然在只有一行落差的场景里，动画早就跑完了，采到的只是尾巴上那几像素。

#### 按整行滚动（`followNewest()`）

用户的要求是"图标 + 名称 + 占用的矩形逐渐现出并自动换行（每行多少个随窗宽自适应），
超出屏幕时以**整行**为单位平滑向下滚动"。落成三个约束：

- **每行几个自适应** →
  `grid-template-columns: repeat(auto-fill, minmax(min(var(--card-min), 100%), 1fr))`。
  `min(…, 100%)` 那层是必须的，否则窄窗口下 `minmax` 的下界会把网格撑破、横向溢出。
- **能按整行滚动** → 行高必须确定，所以 `grid-auto-rows: var(--card-h)` 定高。
  每行等高才有确定的行距可对齐。
- **对齐到行边界 + 平滑** → `followNewest()`。三件事必须一起做，少一件都会看出破绽：

  1. **量行距用 `offsetHeight` / `offsetTop`，不用 `getBoundingClientRect`**。新卡片正
     带着入场动画（`translateY(10px) scale(0.96)`），rect 返回的是**动画中的**几何，
     `scale(0.96)` 会把 95px 的卡片量成 91px，行距随之算小、对齐全偏。`offset*` 是布局
     值，不受 transform 影响。
  2. **对齐要带上 `padding-top`**。行顶边在 `padding-top + k * 行距` 处，按纯
     `k * 行距` 对齐的话视口顶部会切掉小半行。顶部区域现在挪到了 `#cards` 外面，所以
     这个 `padTop` 是 **0** —— 但公式照旧得带上它，否则以后谁再往里加内边距就又错了。
  3. **底部内边距动态补足**，让最大滚动量正好等于对齐后的目标位置。不补的话目标超过
     最大滚动量会被浏览器夹回去，对齐白做 —— 而且最后一行（正是"自动跟随最新"最该看清
     的那一行）会被视口底部切掉一截。补出来的量小于一个行距，又落在最后一行下方，视觉上
     看不出来。基准 `padding-bottom` 记在 `cards.dataset.basePadBottom`（只认第一次读到
     的值），算溢出时先减掉当前补量，这样它就跟当前 padding 无关、不会来回震荡。

  目标是"最后一行完整可见 + 视口顶部落在行顶边"这两个条件的**最小**解，所以内容每多一行
  目标正好前进一个行距，看上去就是整行整行往上走。

- **自动跟随**：往下跟最新一行；用户**往上滚**就停（按 `scrollTop` 的方向判断，
  `top < lastScrollTop - 2`），滚回底部（`overflow - top <= FOLLOW_SLACK`，24px）自动
  恢复。不用 `scrollend`、也不用去区分平滑滚动的中间帧 —— 自动跟随永远向下滚，所以
  "往上"必定是用户干的。
- **尺寸变化后重新对齐**：用 `ResizeObserver` 盯 `#cards` 自己的盒子，而不是
  `window.resize`。前者覆盖面更广（分屏、WebView 自己改尺寸都算），回调本来就按帧合并。
  改 `padding-bottom` **不会**反过来触发它 —— `#cards` 的高度由 flex 决定，内边距变了
  盒子尺寸也不变，不会自己喂自己。

#### 换肤与"缓缓浮现"

- **换肤靠 CSS 变量，不是加遮罩**。深色主题的对比度压在喜报上根本不够用，所以
  `html.classic` 直接覆盖整套 `--bg / --panel / --panel-solid / --sheet / --text /
  --muted / --accent / --field / --control / --th / --row-hover / --row-line`。代价是
  `styles.css` 里不能再有写死的颜色 —— 原来那几处 `#1a1c21`、`#2a2e36` 都提成了变量。
  面板透明度留在 0.78~0.84：再厚一点喜报就糊成背景噪声。
- **`--panel-solid` 是给"浮在图案上"的元素用的**（选择页面板、经典模式左上角那两个图标
  胶囊）：喜报正中最亮的那块是纯黄，0.8 的米黄压不住它，文字会发飘，所以这几个元素用
  0.93 的更实底色。深色主题下 `--panel-solid` 与 `--panel` 同值。
- **背景图必须在 `frontendDist` 里面**（所以放在 `ui/assets/`，不是仓库根的 `assets/`）。
  Tauri 只服务 `ui/`，放外面 `<img src>` 根本取不到。放在 `ui/` 下的额外好处是它会被
  `tauri-codegen` 一起内嵌进 exe，运行时不需要外部文件。
- **背景图用有损 WebP q85（122 KB），不是无损**。关键在于 `tauri-codegen` 是**原样嵌入**
  —— 把 `ui/` 下每个文件当字节数组塞进 exe，不做任何二次压缩。所以这张图多大，
  `cefscanw.exe` 就白白大多少。最初放的是 736 KB 的无损 WebP，占了当时 5.73 MB exe 的
  13%；换成 q85 后 exe 降到 5.10 MB。实测（1000×749 RGB，渐变 + 文字的海报）：

  | 方案 | 字节 | 说明 |
  | --- | ---: | --- |
  | 无损 WebP | 736.0 KB | 原方案 |
  | **WebP q85** | **122.4 KB** | 现方案 |
  | WebP q90 / q80 | 157.0 / 100.5 KB | 相邻档位 |
  | PNG 24bit | 917.6 KB | **反而更大**：渐变 + 文字的无损通道压不动 |
  | PNG 256 色 | 342.8 KB | 有量化色带 |
  | JPEG q90（4:4:4） | 279.2 KB | 比 WebP 大一倍，且文字边缘有振铃 |

  也就是说，**PNG 和 JPEG 在这张图上都不划算**，有损 WebP 是唯一的选择。转码脚本
  `tools/compress_background.py`（幂等：已经是 `VP8 ` 就跳过，避免二次有损劣化），
  原图备份在 `.workbuddy-ai/assets-backup/` —— 那里被 `.gitignore` 排除，所以不会被
  `cargo clean` 清掉，而原图本身也没进 git。
- **别用 RGB PSNR 判断 WebP 有损的画质**。q85 的 RGB PSNR 只有 31.58 dB，看着像明显
  劣化，但拆开看是：亮度 Y **40.36 dB**、Cb 34.51 dB、Cr 35.30 dB。RGB 的算法把色度
  误差按和亮度一样的权重摊了进来，而人眼对色度的分辨率低得多 —— 这正是 JPEG/WebP 敢对
  色度做 4:2:0 抽样的前提。看亮度的那个数才和观感对得上。顺带记一条：
  `save(..., subsampling="4:4:4")` 对 WebP 是**无效参数**，Pillow 静默忽略。
- **最终判据是渲染后的截图差分，不是裸图指标**。同一份页面分别用无损原图和 q85 渲染、
  截图、逐像素差分，结果是**亮度 PSNR 53.12 dB、最大亮度差 11/255、差 >8 的像素占
  0.00%**。比裸图的 40 dB 还好 —— 因为半透明面板（0.78~0.84 alpha）把差异吸收掉了。
- **"缓缓出现"要排队，不能收到就画**。索引后端会在几百毫秒内一次吐出几十条，直接画出来
  是一整屏同时"啪"地出现。所以经典模式下结果先进 `revealQueue`，由 `revealTick` 按固定
  节奏（`REVEAL_STEP_MS = 150`）搬进 `rows`。`step` 由两个数夹出来：
  `min(ceil(pending * STEP / REVEAL_BUDGET_MS), REVEAL_MAX_PER_TICK)`。
- **汇总要等队列排空**（`deferredDone`）。不然会出现"已完成，共 8 个"和还在往外浮的结果
  同框。经典模式没有汇总栏，它的结果显示是顶部那行条数，所以汇总栏和这条状态行**只服务
  选择页和工具模式**。
- **入场动画只给"还没画过"的元素**（`painted`）。`render()` 每次都重建整个 `innerHTML`，
  不加这个标记的话，排序、展开、来新结果都会让整墙重放一次动画。非经典视图的 `pushRow`
  直接把 `painted` 置为 `true`，一帧动画都不做。
- **动画挂在 `html.classic .card.enter` 上**，并且 `@media (prefers-reduced-motion:
  reduce)` 里关掉。原来表格那套 `tbody tr.enter` / `@keyframes row-enter` 已经删掉 ——
  经典模式改卡片墙之后它永远匹配不到可见行，是死代码。

**一个踩过的坑**：`.summary { display: flex }` 和浏览器默认的 `[hidden]
{ display: none }` 优先级一样，但作者样式永远压过默认样式 —— 所以只写 `hidden` 属性是
藏不住的，汇总栏会在开扫之前就顶着"应用 0 / 总占用 0 B"露出来。`styles.css` 顶部因此加了
一条 `[hidden] { display: none !important; }`。（这条现在更关键：三个视图全靠 `hidden`
切换。）

### 5.3 工具模式

工具栏 + 汇总栏 + 表格。表格的能力：图标列 + 名称列 + 类型 + 占用 + 运行 + 路径（可排序）、
点击行展开完整路径并在资源管理器中定位（`explorer /select,"path"`，路径带空格必须加引号）。
表格**不再有入场动画**（"缓缓浮现"是卡片墙的事）。

## 6. 路径折叠

按**分隔符切段**折叠，不是按字符数切 —— 前面只留「根 + 3 层目录」（`PATH_HEAD_SEGMENTS`），
中间省略号，后面只留文件名，这样尾部一定是完整的文件名：

```
C:\Users\16695\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe
  → C:\Users\16695\AppData\…\WorkBuddy.exe
```

盘符（`C:`）和 UNC 的空段都算"根"，不占目录层数，所以
`\\server\share\dir\sub\file.exe` 折成 `\\server\share\…\file.exe`。折完不比原文短就
返回原文。

表格用 `table-layout: fixed` + `<colgroup>` 固定各列宽度、路径列吃剩余空间，这样折叠后的
文本不会再被 CSS 的 `text-overflow` 二次截断（否则尾部会被吃掉）。

## 7. 图标资源

`tools/make_icon.py` 生成并入库两个文件，**缺一不可**：

| 文件 | 谁要 | 备注 |
| --- | --- | --- |
| `icons/icon.ico` | Windows：`tauri-build` 生成资源文件 | 6 个尺寸的 32bpp BMP 条目 |
| `icons/icon.png` | Unix：`tauri-codegen` 取默认窗口图标 | 256×256，**必须 RGBA** |

`tauri.conf.json` 的 `bundle.icon` 同时列了这两个；`tauri-build` 按 `.find(|i|
i.ends_with(".ico"))` 挑、`tauri-codegen` 按 `.png` 挑，互不干扰。

这里有个**只在 Linux 上才暴露的坑**：`tauri-codegen` 的 `find_icon` 在非 Windows 目标上
从 `bundle.icon` 里挑第一个 `.png`，挑不到就退回硬编码的 `icons/icon.png`，再找不到就在
`generate_context!` 里 panic（"failed to open icon ...: No such file or directory"）。
Windows 走的是另一条路（`default_window_icon_from_app_icon_resource`），所以**本地和
Windows CI 都验证不到**，第一次推 CI 就是 5 分钟后才炸在 Linux job 上。另外
`CachedIcon::new_png` 会检查 `png::ColorType::Rgba`，RGB 或调色板同样 panic。

`tools/check_icons.py` 把这两条约束抽出来做静态校验（复刻 `find_icon` 的挑选语义），
CI 的 lint job 第一步就跑它，秒级报错。

## 8. 前端的验证（桩 / 截图 / 端到端）

**三层验证，都不能省**：

1. `node tools/ui_harness.js` —— 拿一个几百行的 DOM 桩把 `ui/main.js` 跑起来，喂进假事件，
   断言的是**调用次数和时序**：揭示队列搬了几条、`painted` 有没有防住重放、卡片墙自动
   跟随的目标有没有对齐到整行、底部内边距补得对不对、进工具模式是不是立刻探测后端、
   发给后端的请求长什么样、**工具页的排序有没有串到卡片墙**。
   **124 项，半秒跑完，不需要 npm**，已接进 CI 的 lint job。
   - 它抓到过一个**真 bug**：在途的后端探测会在扫描失败之后把"未确定"盖回成一个后端名
     （见 §2 的 `backendEpoch`）。这种竞态在真机上要凑时机才复现，桩里只要控制 promise
     的 resolve 顺序就能稳定打出来。
   - 它还盯着**跨文件的不变量**：卡片几何三件套是否同步缩小（读 `styles.css`）、图标槽
     尺寸是否与 `cardIconHtml` 一致。**负面断言（"不该再出现 X"）一律对剥离注释后的文本
     做**（`stripComments`）—— 否则注释里解释"为什么不用 X"会把断言绊倒。
   - **写完新断言要用"旧实现"反跑一次确认它会失败**，否则数据同序、代码路径没走到之类的
     原因会让它恒真，等于写了个装饰（踩过：卡片墙顺序断言第一版就是怎么改都全过）。
2. `python tools/preview_ui.py <输出目录> [picker|classic|tool] [条数]` —— 生成一份带假数据
   的静态预览页（把 `ui/` 整个抄过去，再塞一个假的 `window.__TAURI__`），无头浏览器截图。
   **一次只出一个视图**，三视图要跑三次。桩**不能替代**截图：真实 DOM 的布局和 CSS 层叠
   它完全看不见，`[hidden]` 被 `.summary { display: flex }` 压掉那个 bug 就只有截图才发现
   得了。
3. `python tools/gui_smoke.py <out.png> [--mode classic|tool] [--root <dir>] <秒…>` —— 手动
   端到端，真去点窗口，验证「Tauri command + Channel + 前端渲染」这条链。

### `preview_ui.py` 的四个坑

① 桩必须整体包在 IIFE 里（经典脚本的顶层 `class Channel {}` 会占住全局词法作用域的名字，
而 `main.js` 顶层写的正是 `const { invoke, Channel } = …`，会以"Identifier 'Channel' has
already been declared"整体解析失败，表现只是"点了按钮没反应"）；
② Python 的 `True` / `False` 不是 JavaScript 字面量；
③ **无头截图会把视口放大**，这是最坑的一个：`--window-size=1280,800` 下页面看到的是
1264×705，而截图输出是 1280×800，并且页面**收不到 resize 事件**（`innerHeight` 始终
705，`ResizeObserver` 也不触发）—— 是合成层重排，不是布局事件。后果是截图那一刻最大滚动
量变小、`scrollTop` 被夹回，顶部凭空切掉 95px。**这是截图工具的假象，不是前端 bug**：
同一时刻的 `--dump-dom` 显示 `scroll=1170/1170`、对齐残差 0。所以经典模式截图前先**滚到
顶**（`scrollTop=0` 不会被夹取，状态可确定），并用它当独立判据量出行顶边落在
`0 / 106 / 212 / 318 / 424 / 530`（行距 = `--card-h` 95 + `--card-gap` 11；顶部区域已挪到
`#cards` 外面，所以 `padTop` 是 0）；
④ **driver 不要用定长 sleep 等揭示结束，要轮询**。原来写的是 `setTimeout(report, 6000)`，
那时揭示有 4 秒预算兜底、勉强够；加了"一拍最多 3 张"的上限之后，总时长变成跟条数成正比
（60 张要 9 秒），定长等待就要么白等、要么截到"还在往外浮"的中间态 —— **表现是 title
停在 `cefscanw`**，或者卡片只出来一小半（一眼看不出是工具的问题）。现在 driver 轮询刷新
胶囊是否恢复可用（`scanning` 一挂上它就 disabled），`SETTLE_MS` 只当超时上限。另外
`--virtual-time-budget` 必须**大于**揭示总时长，它和 `SETTLE_MS` 一起调 —— 少给一个就会
截到中间态。chrome 的完整命令行见 `preview_ui.py` 模块文档。

### `gui_smoke.py` 的要点

- **必须硬性置顶**（`SetWindowPos(HWND_TOPMOST)`）。脚本抓的是**屏幕**，只调
  `SetForegroundWindow` 的话，Windows 允许前台进程拒绝让出前台权，从终端里跑经常静默
  失败，窗口还压在终端后面 —— 于是抓回一整张终端内容，还会因为终端里的蓝色链接文字匹配
  上强调色而"找到"一个假按钮。症状是"整窗都是同一种深灰"，非常难判断。收尾有 `unpin()`
  取消置顶。
- **找按钮：二维连通域 + "够宽、够高、够实心"**。选择页是深色，按钮是蓝的 `#4f8ff7`
  （旧注释里"经典模式的按钮是中国红 `#c31c12`"已经过时 —— 中国红现在只出现在卡片悬停
  描边上）。这个蓝同时出现在好几处，判据要一步步收紧，这里踩了两个坑：
  - **不能按 x 方向投影聚类**。投影忽略 y，标题文字、模式卡描边、按钮三者在 x 上互相
    重叠、间隔都小于 6px，会被并成**同一个簇** —— 那个簇的外接矩形 479x370、填充率 0.09，
    于是按钮被连坐判掉。得改成二维连通域（8 邻接 flood fill）。**这个坑预览截图验不出来**：
    截图只验"长什么样"，根本不跑这个函数。
  - **光有填充率还不够，还得有最小高度**。选中那张模式卡的上下描边是两条独立的 `340x1`
    连通域，填充率 **1.00** 而且比按钮更宽（按钮实测 `222x63`、填充率 0.92），只按宽度取
    最大就会选中一条 1px 的横线。所以是"宽 >= 60 且 高 >= 20 且 填充率 >= 0.5"，最后按
    **面积**取最大。
- **模式用键盘选**：`Tab` 进单选钮组（一组单选钮里只有被选中的那个是 tab stop，DOM 里
  第一个可聚焦元素就是它），`Down` 在组内切到工具模式。按像素找一个十几像素、还跟背景
  撞色的圆点基本靠运气。
- **工具栏的目录输入框也用键盘**：`Tab` 两次（返回按钮 → 输入框）。**不按像素找**：它的
  底色 `--field` 和工具栏面板 `--panel` 只差 11 个色阶，容差收到 2 都分不开。代价是
  **绑定了 tab 顺序** —— 工具栏里在输入框之前新增可聚焦元素时，这个次数要跟着加。
- **`--root` 只对工具模式有效**：选择页和经典模式都没有目录输入框，所以第一轮扫描永远是
  全盘；`--root` 的用法是等第一轮扫完，把路径打进工具栏输入框、回车重扫，另存一张
  `*_filtered.png`。经典模式里给了只会打印一句提醒。
- **`CEFSCAN_SMOKE_EXPAND` 只对工具模式有效**：经典模式里点卡片是在资源管理器里定位，
  冒烟测试不该真去开一个窗口。
- **抓帧要能重试**。刚置顶之后 DWM 有一小段时间还没合成完，`BitBlt` 会抓回一帧不完整的
  画面 —— 实测遇到过"表格和复选框都在、唯独按钮那块是空的"。这种帧偶发，所以
  `locate_start_button` 失败时会重新聚焦再抓一次（最多 3 次），而不是直接判失败。

想直接验画质差异时，还有一招：同一份页面分别用两个版本的背景图渲染、无头截图、逐像素
差分。这比看裸图的 PSNR 靠谱得多（见 §5.2 换肤一节的结论）。
