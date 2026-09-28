//! 扫描结果的数据模型。

use std::path::PathBuf;
use std::time::Duration;

/// 应用所属的 Chromium 内核类型。
///
/// 用 enum 而非字符串：`rank` 决定同一目录命中多条签名时谁胜出，
/// `label` 是稳定的对外展示/序列化名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum AppKind {
    Electron,
    Edge,
    Chrome,
    Nwjs,
    CefSharp,
    MiniElectron,
    MiniBlink,
    Cef,
    Unknown,
}

impl AppKind {
    /// 优先级。数值越大越强，参考实现中经过实践验证的排序。
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

    /// 取更强的那个（`None` 视为最弱）。
    pub fn strongest(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

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
    /// `libnode.dll` 之类，MiniElectron / MiniBlink 的线索。
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
    /// 结果按大小降序排列（否则按路径升序，保证输出确定）。
    pub sort_by_size: bool,
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
            sort_by_size: true,
            detect_running: true,
        }
    }
}

/// 一次扫描的汇总信息。
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ScanStats {
    /// 实际使用的后端名。
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
