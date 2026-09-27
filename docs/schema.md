# 输出 schema

`cefscan` 与 `cefscanw` 共用同一套数据结构（`cefscan-core::AppInfo`），
JSON / NDJSON / CSV / TOML 四种格式只是同一数据的不同编码。

## 字段

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `path` | string | 展示路径。优先可执行文件，其次是应用根目录 |
| `root` | string | 计量与去重所用的根目录 |
| `kind` | string enum | 内核类型，见下表 |
| `size` | integer | `root` 目录的磁盘占用（字节） |
| `running` | bool | 是否有进程正在运行该可执行文件 |
| `evidence` | string \| null | 命中的签名串；按类型名判定（Edge/Chrome）时为 `"filename"` |

`evidence` 只在有值时出现（JSON 里会被省略，CSV 里为空串）。

## kind 取值

| 值 | 含义 |
| --- | --- |
| `electron` | Electron |
| `edge` | Microsoft Edge |
| `chrome` | Google Chrome |
| `nwjs` | NW.js |
| `cefsharp` | CefSharp（.NET） |
| `mini_electron` | MiniElectron |
| `mini_blink` | MiniBlink |
| `cef` | CEF（libcef） |
| `unknown` | 发现了 Chromium 特征文件，但没匹配到具体签名 |

## 示例

```json
[
  {
    "path": "C:\\Users\\me\\AppData\\Local\\Programs\\App\\App.exe",
    "root": "C:\\Users\\me\\AppData\\Local\\Programs\\App",
    "kind": "electron",
    "size": 734967298,
    "running": true,
    "evidence": "third_party/electron_node"
  }
]
```

## 稳定性承诺

1.0 之后：

- 不删除、不重命名字段，不改变类型；
- 新增字段必须是可选的，且放在对象末尾；
- 新增 `kind` 取值属于**破坏性变更**，会等到主版本升级。

`size` 的两种口径：列表逐条求和是 `sum_bytes`；去掉"被其它根包含"的目录后是
`total_bytes`。两者不同时，说明有应用目录存在嵌套，CLI 会同时打印这两个值。
