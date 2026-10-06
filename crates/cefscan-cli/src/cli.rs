//! 命令行参数。

use std::path::PathBuf;

use cefscan_core::{AppKind, Backend, Direction, ParseAppKindError, ScanOptions, SortKey};
use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendArg {
    /// 优先索引后端（Windows 上的 Everything IPC），不可用则回落到 cefscan 遍历
    Auto,
    /// 只用 cefscan 遍历后端
    Cefscan,
    /// 只用索引后端
    Index,
}

impl From<BackendArg> for Backend {
    fn from(value: BackendArg) -> Self {
        match value {
            BackendArg::Auto => Self::Auto,
            BackendArg::Cefscan => Self::Filesystem,
            BackendArg::Index => Self::Index,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortArg {
    /// 按磁盘占用
    Size,
    /// 按展示路径
    Path,
    /// 按内核类型优先级
    Kind,
}

/// 版本号，编译期由 `CEFSCAN_BUILD_VERSION` 注入，缺省回落到 Cargo.toml 的 workspace version。
const VERSION: &str = match option_env!("CEFSCAN_BUILD_VERSION") {
    Some(value) => value,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Parser)]
#[command(
    name = "cefscan",
    version = VERSION,
    about = "找出电脑上所有基于 Chromium 内核（CEF / Electron / NWJS / CefSharp ...）的应用",
    long_about = "扫描文件系统，找出 CEF、Electron、NWJS、CefSharp、Edge、Chrome 应用，\n并给出它们的磁盘占用与是否正在运行。"
)]
pub struct Cli {
    /// 只扫描这些目录；可重复，不指定则扫描所有盘符
    #[arg(long, value_name = "DIR")]
    pub root: Vec<PathBuf>,

    /// 搜索后端
    #[arg(long, value_enum, default_value = "auto")]
    pub backend: BackendArg,

    /// 遍历线程数（0 = 自动，默认上限 8）
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub threads: usize,

    /// 输出格式
    #[arg(short = 'f', long, value_enum, default_value = "table")]
    pub format: crate::output::Format,

    /// 写入文件而不是 stdout
    #[arg(short = 'o', long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// 只列出这些类型；可重复，如 electron、cef、nwjs
    #[arg(short = 'k', long = "kind", value_name = "KIND")]
    pub kinds: Vec<String>,

    /// 排除目录名；可重复（默认已排除 node_modules / target / 回收站）
    #[arg(long = "exclude-dir", value_name = "NAME")]
    pub exclude_dirs: Vec<String>,

    /// 排除路径；可重复
    #[arg(long = "exclude-path", value_name = "PATH")]
    pub exclude_paths: Vec<PathBuf>,

    /// 只显示正在运行的应用
    #[arg(long)]
    pub running_only: bool,

    /// 只显示占用不小于此值的应用，如 512MB、2GiB
    #[arg(long, value_name = "SIZE")]
    pub min_size: Option<String>,

    /// 排序依据
    #[arg(long, value_enum, default_value = "size")]
    pub sort: SortArg,

    /// 升序排列（默认：占用 / 类型降序，路径升序）
    #[arg(long)]
    pub ascending: bool,

    /// 包含隐藏目录
    #[arg(long)]
    pub include_hidden: bool,

    /// 不检测运行中进程（可略微加快扫描）
    #[arg(long)]
    pub no_running: bool,

    /// 输出命中的签名与使用的后端
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// 只输出结果，不打印汇总
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

impl Cli {
    pub fn scan_options(&self) -> ScanOptions {
        let sort = match self.sort {
            SortArg::Size => SortKey::Size,
            SortArg::Path => SortKey::Path,
            SortArg::Kind => SortKey::Kind,
        };
        let mut options = ScanOptions {
            roots: self.root.clone(),
            backend: self.backend.into(),
            walk_threads: self.threads,
            scan_threads: self.threads,
            include_hidden: self.include_hidden,
            detect_running: !self.no_running,
            sort,
            sort_direction: if self.ascending {
                Direction::Asc
            } else {
                sort.default_direction()
            },
            ..ScanOptions::default()
        };
        options.exclude_dir_names.extend(self.exclude_dirs.clone());
        options.exclude_paths.extend(self.exclude_paths.clone());
        options
    }

    /// 解析 `--kind` 过滤条件，无法识别的类型直接报错。
    pub fn kind_filter(&self) -> Result<Vec<AppKind>, ParseAppKindError> {
        self.kinds.iter().map(|raw| parse_kind(raw)).collect()
    }

    pub fn min_size_bytes(&self) -> Result<Option<u64>, String> {
        self.min_size.as_deref().map(parse_size).transpose()
    }
}

/// 解析 `--kind` 的取值。
///
/// # Errors
///
/// 取值不在 [`AppKind::ALL`] 里时返回 [`ParseAppKindError`]，它的 `Display`
/// 会列出全部合法取值（由 `ALL` 生成，不会和实际支持的取值脱节）。
pub fn parse_kind(raw: &str) -> Result<AppKind, ParseAppKindError> {
    raw.parse()
}

/// 解析 `512MB` / `2GiB` / `1048576` 这类写法。
pub fn parse_size(raw: &str) -> Result<u64, String> {
    let text = raw.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("invalid size `{raw}`"))?;
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        other => return Err(format!("unknown size unit `{other}` in `{raw}`")),
    };
    Ok((number * multiplier).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_units_are_binary() {
        assert_eq!(parse_size("512MB").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_size("2GiB").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("2048").unwrap(), 2048);
        assert_eq!(parse_size("1.5 KiB").unwrap(), 1536);
    }

    #[test]
    fn bad_sizes_are_reported() {
        assert!(parse_size("abc").is_err());
        assert!(parse_size("10 parsecs").is_err());
    }

    #[test]
    fn kinds_accept_labels() {
        assert_eq!(parse_kind("electron").unwrap(), AppKind::Electron);
        assert_eq!(parse_kind("Mini_Electron").unwrap(), AppKind::MiniElectron);
        assert!(parse_kind("firefox").is_err());
    }

    /// `--sort` 的三个取值必须映射到三个**不同**的排序键（`kind` 曾经静默退化成 `path`），
    /// 且默认方向随主键而定。
    #[test]
    fn sort_key_and_direction_are_mapped() {
        let options = |args: &[&str]| Cli::try_parse_from(args).unwrap().scan_options();

        assert_eq!(options(&["cefscan"]).sort, SortKey::Size, "默认按占用");

        let size = options(&["cefscan", "--sort", "size"]);
        assert_eq!(size.sort, SortKey::Size);
        assert_eq!(size.sort_direction, Direction::Desc);

        let path = options(&["cefscan", "--sort", "path"]);
        assert_eq!(path.sort, SortKey::Path);
        assert_eq!(path.sort_direction, Direction::Asc, "路径默认升序");

        let kind = options(&["cefscan", "--sort", "kind"]);
        assert_eq!(kind.sort, SortKey::Kind);
        assert_eq!(kind.sort_direction, Direction::Desc, "类型默认降序");

        let ascending = options(&["cefscan", "--sort", "size", "--ascending"]);
        assert_eq!(
            ascending.sort_direction,
            Direction::Asc,
            "--ascending 覆盖主键的默认方向"
        );
    }

    /// `--backend` 的取值集合与顺序（`auto` / `cefscan` / `index`）。
    #[test]
    fn backend_values_and_order_are_fixed() {
        for value in ["auto", "cefscan", "index"] {
            assert!(
                Cli::try_parse_from(["cefscan", "--backend", value]).is_ok(),
                "取值 {value} 应该被接受"
            );
        }

        assert!(Cli::try_parse_from(["cefscan", "--backend", "filesystem"]).is_err());

        let help = Cli::try_parse_from(["cefscan", "--help"])
            .unwrap_err()
            .to_string();
        let position = |needle: &str| {
            help.find(needle)
                .unwrap_or_else(|| panic!("--help 里找不到 {needle}：\n{help}"))
        };
        assert!(
            position("- auto:") < position("- cefscan:")
                && position("- cefscan:") < position("- index:"),
            "--help 里的取值次序应为 auto / cefscan / index：\n{help}"
        );
    }

    /// clap 的取值名（`ValueEnum` 自己拼的字符串）与 [`Backend::label`]（GUI 请求字段用的）
    /// 是两份独立映射，必须永远一致，否则 GUI 传上来的值 CLI 不认、反之亦然。
    #[test]
    fn backend_arg_names_match_backend_labels() {
        for arg in BackendArg::value_variants() {
            let name = arg.to_possible_value().unwrap();
            assert_eq!(
                name.get_name(),
                Backend::from(*arg).label(),
                "{arg:?} 的 clap 取值名与 Backend::label 不一致"
            );
        }
    }
}
