# 测试

> 工程可测试性是本项目两条差异化主线之一（另一条是性能，见 [`performance.md`](performance.md)）。
> 前端的三层验证（DOM 桩 / 截图 / 端到端）见 [`gui.md`](gui.md) §8。

## 1. 测试策略

| 层 | 做法 |
| --- | --- |
| 单元 | 每个模块内联 `#[cfg(test)] mod tests`。签名扫描器对 `impl Read` 工作 → 喂 `Cursor<Vec<u8>>` 构造假 PE / ELF / Mach-O 字节即可测，无需真实二进制、无需落盘。 |
| 夹具（fixture） | 用 `tempfile` 在临时目录里造**假应用树**（Electron 目录、NWJS 目录、纯 CEF 目录、Edge 目录，以及负样本：只有 `unins000.exe` 的目录、`node_modules` 内的 decoy），扫 → 断言。`TempDir` 的 `Drop` 负责清理。 |
| 纯函数优先 | 凡是能抽成平台无关纯函数的规则都抽出来（如 `filter::excluded_root_hit`、`naming::display_name`、`candidate::classify_candidate_name`），这样在任一平台上都能测、也测得出。 |
| 确定性排序 | 输出必须**按路径排序**后再序列化，禁止依赖 `HashMap` 迭代顺序（分组用 `BTreeMap`）。 |

> **与早期计划的偏差**：计划里的 `insta`（快照）、`proptest`（属性测试）、`assert_cmd`
> （CLI 集成）、`criterion`（基准）都**没有引入**。原因是自写遍历 + 纯函数化的启发式让
> 大部分风险都能用内联单测覆盖，而少一个 dev-dependency 就少一份编译时间与版本漂移。
> 需要时再补，不预设。

## 2. 确定性要求

- 输出必须**按路径排序**后再序列化，禁止依赖 `HashMap` 迭代顺序。
- 任何测试不得访问真实全盘；需要真实路径的用例一律 `#[ignore]` 并标注手工运行方式。
- 时间 / 内存相关的断言只出现在基准工具里，不进单测。

## 3. 跨平台测试的硬规矩（第一次跑 Linux CI 换来的）

### 3.1 平台相关的断言必须显式门控

下面这些在 Windows 上必过、在 Linux 上必挂：

| 写法 | 在 Unix 上的结果 |
| --- | --- |
| 用 `C:\...` 字面量当路径 | `\` 不是分隔符，整条串被当成**一个文件名**，`file_stem()` 只剥掉 `.exe` |
| 断言大小写不敏感 | `classify_candidate_name` 在 Linux 上**刻意不小写化**，只有全小写拼写命中 |
| 把 fixture 建在 `std::env::temp_dir()` | Linux 上是 `/tmp`，在 `PLATFORM_EXCLUDED_ROOTS` 里 |
| 目录大小期望值只算文件 | ext4 上目录 `st_size` 是 4096，NTFS 上是 0 |
| 用空 roots 调 `walk()` 验证"不 panic" | 会走平台默认起点（Unix 是 `/`）**真的遍历整个文件系统** |

规矩：**平台相关的断言要么 `#[cfg(target_os = ...)]` 分开写，要么把规则本身抽成平台无关的
纯函数**（`filter::excluded_root_hit` 就是这么来的，纯字符串比较，抽出来之后 Windows 上也
能测）。

### 3.2 别用"跑起来不 panic"当测试

这种断言既抓不到回归，又可能偷偷扫全盘 —— `empty_roots_report_an_error` 就是这么在 Linux
CI 上跑了 115 秒、还一条断言都没有的。要测推导逻辑就直接调 `resolve_roots`。

### 3.3 涉及"没有索引服务"的断言要门控

装了 Everything 的 Windows 机器行为不确定，所以
`auto_backend_falls_back_to_cefscan_and_says_so`、
`index_backend_without_a_service_is_an_error` 这类测试带
`#[cfg(not(all(feature = "everything", target_os = "windows")))]` 门控，只有"没有索引
服务可用"的平台才能断言。

## 4. 第一次真跑 CI 暴露出来的问题（值得留着）

Linux 这一列此前**从来没跑过**，一次就翻出四类只在 Unix 上出现的问题：

| # | 现象 | 根因 |
| --- | --- | --- |
| 1 | Linux 编译在 `generate_context!` panic | 缺 `icons/icon.png`（见 [`gui.md`](gui.md) §7） |
| 2 | `walk` 两个测试：`dirs_scanned` 只有 1 | fixture 建在 `/tmp`，而 `/tmp` 在 Unix 的 `PLATFORM_EXCLUDED_ROOTS` 里 |
| 3 | `size` 测试期望 350 实得 4446 | `dir_size` 把目录 inode 的 `st_size`（ext4 上 4096）也累加了 |
| 4 | `naming` / `candidate` 共 4 个断言 | 硬编码 `C:\...` 字面量 + 断言大小写不敏感 |

第 2 条的修法顺带修掉一个真 bug：Unix 的排除名单原本**无条件**生效，导致
`cefscan --root /tmp/foo` 静默返回空。现在规则改成「被排除的根若落在某个显式 root 之内或
与之相等，则不再排除」，`--root /` 这种等于全盘的写法仍然走名单。排查过程中还发现
`path_starts_with` 在 **root 以分隔符结尾**时（`C:\`、`/`、或用户敲的 `--root "C:\foo\"`）
边界判断失败，`in_roots` 会把整棵子树挡掉 —— 所以 `cefscan --root C:\` 之前也是扫不出
东西的。两处都已修并补了测试。

**教训**：只在主开发平台（Windows）跑测试，是发现不了这四类问题的；反过来，本地也没有能
跑 Linux 测试的环境（没有 WSL / 容器），所以**要么把规则抽成平台无关的纯函数**，**要么就
靠 CI 兜底**。

## 5. Miri：纯字节解析的入口

Miri 跑不了 Win32 FFI，所以它在这类项目里通常没得测。但**协议编解码与 magic 判定是纯字节
处理**，可以整段交给 Miri：

- `scan/everything_codec.rs` —— Everything IPC 的编解码，**平台中立**（不依赖任何 Windows API）。
  真实调用点只有 Windows 的 `scan/everything.rs`，但这一层在**测试构建里所有平台都编译**，
  于是编解码测试在 Linux / macOS 的 `cargo test` 里也跑得到 —— 这本身修掉了一个覆盖缺口
  （以前这些测试只在 Windows 上跑）。
- `signature::is_executable_magic` —— ELF / PE / Mach-O 的 magic 判定。

两处各有一个 `#[cfg(miri)]` 的对抗性入口，普通 `cargo test` 不跑（Miri 慢两三个数量级）：

```bash
rustup toolchain install nightly --profile minimal --component miri,rust-src
cargo +nightly miri test -p cefscan-core --lib -- miri_
```

- `everything_codec::tests::miri_malformed_replies_never_panic`：长度取 14 个关键边界 × 5 种填充，
  每条再把文件名偏移指到**每一个**可能位置（含落在头部、奇数、越界），确认 `read_u32` /
  `read_utf16_z` 的边界检查都兜得住。
- `signature::tests::miri_executable_magic_never_panics`：长度 0..8 × 7 种首字节，加各 magic 的
  前缀（"长度够但内容不对"与"内容对但长度不够"两侧都覆盖）。

**别把整个 core 丢给 Miri**：`cargo +nightly miri test -p cefscan-core --lib` 会在
`group::inspect_parallel` 的 rayon 线程上直接报错退出（Miri 不支持真实并发），`signature` 的
扫描器测试也慢到 6 分钟以上跑不完。范围就限定在上面这两条 `miri_` 前缀的入口。

## 6. 尚未做（按优先级排）

- `cargo llvm-cov` 覆盖率，core crate 门槛先定 70%。
- `cargo deny check`（license + advisory）。
- `cargo +1.92.0 check` 显式验 MSRV。现在靠 `rust-version` 字段兜底（toolchain 低于该版本
  时 cargo 直接报错），CI 用的是 `@stable`。
