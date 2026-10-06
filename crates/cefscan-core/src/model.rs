//! 扫描结果的数据模型。

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// 应用所属的 Chromium 内核类型。
///
/// 刻意不加 `#[non_exhaustive]`：整个 workspace 同版本一起发，新增变体时让下游的 `match`
/// 直接编译不过，比静默落进 `_` 分支安全（理由见 `docs/agent/decisions.md` §5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum AppKind {
    Electron,
    Edge,
    Chrome,
    Nwjs,
    /// `label()` 是 `cefsharp`（`docs/schema.md` 冻结的取值），但 `rename_all = "snake_case"`
    /// 会把 `CefSharp` 拆成 `cef_sharp`。这里显式覆盖 —— 前端 `KIND_COLORS` 与 `--kind`
    /// 都按 `cefsharp` 认，用派生值就会让 CefSharp 在 GUI 里静默掉成 unknown 的灰色。
    #[cfg_attr(feature = "serde", serde(rename = "cefsharp"))]
    CefSharp,
    MiniElectron,
    MiniBlink,
    Cef,
    Unknown,
}

impl AppKind {
    /// 全部变体，按 [`Self::rank`] 从强到弱排列。
    ///
    /// `--kind` 的取值解析与报错列表都由它生成，所以新增变体时必须同步这里。
    /// `label` / `rank` 的穷尽 `match` 会先逼你改，但漏掉 `ALL` 编译器不会吭声 ——
    /// `model.rs` 的 `all_lists_every_variant` 测试就是补这个洞的。
    pub const ALL: [Self; 9] = [
        Self::Electron,
        Self::Edge,
        Self::Chrome,
        Self::Nwjs,
        Self::CefSharp,
        Self::MiniElectron,
        Self::MiniBlink,
        Self::Cef,
        Self::Unknown,
    ];

    /// 优先级，数值越大越强。
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Electron => 100,
            Self::Edge | Self::Chrome => 95,
            Self::Nwjs => 90,
            Self::CefSharp => 80,
            Self::MiniElectron => 75,
            Self::MiniBlink => 70,
            Self::Cef => 60,
            Self::Unknown => 0,
        }
    }

    /// 对外展示名，同时是 JSON / CSV 里的序列化值。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Electron => "electron",
            Self::Edge => "edge",
            Self::Chrome => "chrome",
            Self::Nwjs => "nwjs",
            Self::CefSharp => "cefsharp",
            Self::MiniElectron => "mini_electron",
            Self::MiniBlink => "mini_blink",
            Self::Cef => "cef",
            Self::Unknown => "unknown",
        }
    }
}

impl std::str::FromStr for AppKind {
    type Err = ParseAppKindError;

    /// 解析 `--kind` 的取值：忽略首尾空白，大小写不敏感。
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let wanted = raw.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|kind| kind.label() == wanted)
            .ok_or_else(|| ParseAppKindError {
                input: raw.to_owned(),
            })
    }
}

/// 解析不出内核类型名时的错误。`Display` 里列出全部合法取值（由 [`AppKind::ALL`] 生成，
/// 不会和实际支持的取值脱节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseAppKindError {
    input: String,
}

impl fmt::Display for ParseAppKindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown kind `{}` (expected one of: ",
            self.input
        )?;
        for (index, kind) in AppKind::ALL.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            formatter.write_str(kind.label())?;
        }
        formatter.write_str(")")
    }
}

impl std::error::Error for ParseAppKindError {}

/// 一个被识别出来的应用。
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AppInfo {
    /// 展示路径：优先可执行文件，其次应用根目录。
    pub path: PathBuf,
    /// 用于计量与去重的根目录。
    pub root: PathBuf,
    pub kind: AppKind,
    /// 根目录的磁盘占用（字节）。
    pub size: u64,
    /// 是否有进程正在运行该可执行文件。
    pub running: bool,
    /// 命中的签名串，仅诊断用；没有时为 `None`。
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub evidence: Option<&'static str>,
}

/// 单个候选文件，由遍历阶段产出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub kind: CandidateKind,
}

/// 候选文件按文件名分出的类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateKind {
    /// `chrome_100_percent.pak` 之类的 Chromium 资源包。
    Pak,
    /// `libcef.dll` / `Electron Framework` 之类的内核本体。
    Cef,
    /// `libnode.dll` 之类，MiniElectron / `MiniBlink` 的线索。
    Node,
}

/// 搜索后端选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Backend {
    /// 优先索引后端，失败则回落文件系统遍历。
    #[default]
    Auto,
    /// 只用索引后端（Windows 上是 Everything IPC）。
    Index,
    /// 只用文件系统遍历。
    Filesystem,
}

impl Backend {
    /// 全部取值，顺序与 CLI `--backend` 的 `--help` 一致。
    pub const ALL: [Self; 3] = [Self::Auto, Self::Filesystem, Self::Index];

    /// 对外取值名。CLI 的 `--backend` 与 GUI 的请求字段都用这一套。
    ///
    /// 注意 `Filesystem` 的取值是 `cefscan`（遍历后端自己的名字，见 [`FILESYSTEM_BACKEND`]），
    /// 不是 serde 派生出来的 `filesystem` —— 后者只是 `Backend` 自身被序列化时的形态，
    /// 没人拿它当参数传。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Index => "index",
            Self::Filesystem => FILESYSTEM_BACKEND,
        }
    }

    /// 从取值名解析。认不出来返回 `None`，由调用方决定是报错还是回落默认值
    /// （CLI 报错，GUI 回落 `Auto`）。
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|backend| backend.label() == label)
    }
}

/// 结果排序依据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// 按磁盘占用。
    #[default]
    Size,
    /// 按展示路径。
    Path,
    /// 按内核类型优先级（见 [`AppKind::rank`]）。
    Kind,
}

impl SortKey {
    /// 该主键的自然方向，也就是没给 `--ascending` 时的默认值。
    ///
    /// 占用与类型都是「越大越靠前」更有用，路径则按字典序读起来更顺
    /// （`--sort path` 曾经就是路径升序，保持不动）。
    #[must_use]
    pub const fn default_direction(self) -> Direction {
        match self {
            Self::Size | Self::Kind => Direction::Desc,
            Self::Path => Direction::Asc,
        }
    }
}

/// 排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// 降序：主键越大越靠前。
    #[default]
    Desc,
    /// 升序：主键越小越靠前。
    Asc,
}

/// 扫描参数。
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// 遍历起点；为空时自动推导（Windows 取所有逻辑盘，其它平台取 `/`）。
    pub roots: Vec<PathBuf>,
    pub backend: Backend,
    /// 全平台生效的目录名排除（大小写不敏感）。
    pub exclude_dir_names: Vec<String>,
    pub exclude_paths: Vec<PathBuf>,
    pub include_hidden: bool,
    pub follow_symlinks: bool,
    /// 遍历线程数，0 表示自动。
    pub walk_threads: usize,
    /// 签名扫描与体积统计的并行度，0 表示自动。
    pub scan_threads: usize,
    /// Everything IPC 超时。
    pub index_timeout: Duration,
    /// 结果排序依据。
    pub sort: SortKey,
    /// 排序方向。次级键恒为路径升序，不受方向影响。
    pub sort_direction: Direction,
    /// 是否检测运行中进程。
    pub detect_running: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            backend: Backend::default(),
            exclude_dir_names: vec![
                "node_modules".into(),
                "target".into(),
                "$Recycle.Bin".into(),
                "System Volume Information".into(),
            ],
            exclude_paths: Vec::new(),
            include_hidden: false,
            follow_symlinks: false,
            walk_threads: 0,
            scan_threads: 0,
            index_timeout: Duration::from_millis(1500),
            sort: SortKey::default(),
            sort_direction: Direction::default(),
            detect_running: true,
        }
    }
}

/// 遍历后端的展示名。
pub const FILESYSTEM_BACKEND: &str = "cefscan";

/// 一次扫描的汇总信息。
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ScanStats {
    /// 实际使用的后端名，取值见 [`FILESYSTEM_BACKEND`] 或索引服务的名字。
    pub backend: &'static str,
    /// 遍历到的目录数。
    pub dirs_scanned: u64,
    /// 命中的候选文件数。
    pub candidates: usize,
    /// 应用条数。
    pub apps: usize,
    /// 列表求和口径的总占用。
    pub sum_bytes: u64,
    /// 去重口径的总占用（去掉被其它根包含的目录）。
    pub total_bytes: u64,
    /// 扫描耗时。
    pub elapsed_ms: u64,
}

/// 扫描过程中的阶段性通知。
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ScanNotice {
    /// 实际使用的后端名，取值与 `ScanStats::backend` 同源。
    pub backend: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` 是 `label()` / `rank()` 之外的第二份清单，用这个测试把它钉住：
    ///
    /// - 下面的 `match` 没有 `_` 分支，新增变体时这里编译不过，逼着同步 `ALL`
    ///   （否则 `--kind` 会静默认不出新类型，报错信息里也不会列它）；
    /// - 顺带验证 `ALL` 真的是按 rank 从强到弱排的，`--kind` 的报错列表读起来才有序。
    #[test]
    fn all_lists_every_variant() {
        let expected_label = |kind: AppKind| match kind {
            AppKind::Electron => "electron",
            AppKind::Edge => "edge",
            AppKind::Chrome => "chrome",
            AppKind::Nwjs => "nwjs",
            AppKind::CefSharp => "cefsharp",
            AppKind::MiniElectron => "mini_electron",
            AppKind::MiniBlink => "mini_blink",
            AppKind::Cef => "cef",
            AppKind::Unknown => "unknown",
        };

        assert_eq!(AppKind::ALL.len(), 9, "变体数变了，ALL 也要跟着改");
        for kind in AppKind::ALL {
            assert_eq!(kind.label(), expected_label(kind), "{kind:?} 的 label 变了");
        }
        assert!(
            AppKind::ALL
                .windows(2)
                .all(|pair| pair[0].rank() >= pair[1].rank()),
            "ALL 应按 rank 从强到弱排列：{ALL:?}",
            ALL = AppKind::ALL
        );
    }

    /// `label()` 与 `serde(rename_all = "snake_case")` 是两份手写映射：JSON / CSV 的
    /// `kind` 字段用前者，消费方按后者理解，一旦分叉就是静默错配。
    ///
    /// 这个测试第一次跑就抓到了 `CefSharp` → `cef_sharp`（见 `CefSharp` 上的 `serde(rename)`）。
    #[cfg(feature = "serde")]
    #[test]
    fn labels_match_the_serialized_form() {
        for kind in AppKind::ALL {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::String(kind.label().to_owned()),
                "{kind:?} 的 label 与序列化值不一致"
            );
        }
    }

    /// `--kind` 的解析走 `FromStr`，大小写与首尾空白都要宽容。
    #[test]
    fn kinds_parse_from_their_labels() {
        for kind in AppKind::ALL {
            assert_eq!(kind.label().parse::<AppKind>().unwrap(), kind);
        }
        assert_eq!(
            "  Mini_Electron ".parse::<AppKind>().unwrap(),
            AppKind::MiniElectron
        );

        let error = "firefox".parse::<AppKind>().unwrap_err().to_string();
        assert_eq!(
            error,
            "unknown kind `firefox` (expected one of: electron, edge, chrome, nwjs, cefsharp, \
             mini_electron, mini_blink, cef, unknown)"
        );
    }

    /// `Backend` 的取值名与 `BackendArg` / GUI 请求字段共用一套，必须能往返。
    #[test]
    fn backend_labels_round_trip() {
        for backend in Backend::ALL {
            assert_eq!(Backend::from_label(backend.label()), Some(backend));
        }
        assert!(
            Backend::from_label("filesystem").is_none(),
            "线上取值是 cefscan"
        );
        assert!(Backend::from_label("whatever").is_none());
    }
}
