# 开发者文档

`cefscan-rs` 的开发者文档。用户视角的说明在 [`../user-guide.md`](../user-guide.md)，
输出字段契约在 [`../schema.md`](../schema.md)。

| 文件 | 内容 |
| --- | --- |
| [`architecture.md`](architecture.md) | 目标与非目标、总体架构、Workspace 布局、数据模型、五阶段流水线、Everything IPC、依赖清单、风险 |
| [`gui.md`](gui.md) | 图形界面：前端形态与通信、后端状态标签（chip）、应用名启发式、图标提取、三视图与卡片墙、路径折叠、前端三层验证 |
| [`performance.md`](performance.md) | 性能措施、实测基线（遍历 / 签名扫描）、`fsindex` 评估与否决 |
| [`testing.md`](testing.md) | 测试策略、确定性要求、跨平台硬规矩、CI 暴露的问题 |
| [`ci-release.md`](ci-release.md) | 三个 workflow、dispatch 输入与目标矩阵、发行说明结构、触发通道与版本号、Cargo profile |
| [`decisions.md`](decisions.md) | 参考实现盘点（继承 / 不继承）、里程碑与进度、参考索引 |

## 快速上手

```bash
cargo build --release                     # 产出 target/release/{cefscan,cefscanw}.exe
cargo test --workspace                    # 单测 + doctest
cargo fmt --all --check                   # 格式
cargo clippy --workspace --all-targets -- -D warnings
node tools/ui_harness.js                  # 前端逻辑（DOM 桩，不需要 npm）
python tools/check_icons.py               # 图标齐全且为 RGBA
python tools/ci_check.py                  # 改 workflow 后先静态校验（需 pyyaml）
python tools/release_scripts_check.py     # release.yml 两段计算脚本真跑一遍（需 Git Bash）
```

构建产物目录 `dist/` 与 CI 口径一致：`cefscan.exe` / `cefscanw.exe` / `README.md` /
`LICENSE` 四个文件放一起就是一个自包含目录。

## 几条别踩的线

- **不要重新引入前端打包器**（Node / Vite）。前端是手写原生 HTML/CSS/JS，靠
  `withGlobalTauri` 拿 `window.__TAURI__.core`（见 [`gui.md`](gui.md) §1）。
- **遍历后端不是 `ignore`**。自写 `read_dir` + rayon；`ignore` 与 `fsindex` 都已被实测否决
  （见 [`performance.md`](performance.md)）。
- **`ScanEvent` 的线上格式是前端的唯一契约**，改名 / 漏 camelCase 只会静默失灵，由
  `src-tauri/src/lib.rs` 的 `scan_events_keep_their_wire_format` 钉死。
- **`everything` 是 core 的默认 feature**，别设成可选（见 [`architecture.md`](architecture.md) §6）。
- **平台相关的断言要门控**，否则 Linux CI 上必挂（见 [`testing.md`](testing.md) §3）。
