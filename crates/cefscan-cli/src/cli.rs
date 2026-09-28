//! 命令行参数。

use std::path::PathBuf;

use cefscan_core::{AppKind, Backend, ScanOptions};
use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendArg {
    /// 优先索引后端（Windows 上的 Everything IPC），不可用则回落到 cefscan 遍历
    Auto,
    /// 只用索引后端
    Index,
    /// 只用 cefscan 遍历后端
    Filesystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortArg {
    Size,
    Path,
    Kind,
}

#[derive(Debug, Parser)]
#[command(
    name = "cefscan",
    version,
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

    /// 升序排列（默认降序）
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
        let mut options = ScanOptions {
            roots: self.root.clone(),
            backend: match self.backend {
                BackendArg::Auto => Backend::Auto,
                BackendArg::Index => Backend::Index,
                BackendArg::Filesystem => Backend::Filesystem,
            },
            walk_threads: self.threads,
            scan_threads: self.threads,
            include_hidden: self.include_hidden,
            detect_running: !self.no_running,
            sort_by_size: matches!(self.sort, SortArg::Size),
            ..ScanOptions::default()
        };
        options.exclude_dir_names.extend(self.exclude_dirs.clone());
        options.exclude_paths.extend(self.exclude_paths.clone());
        options
    }

    /// 解析 `--kind` 过滤条件，无法识别的类型直接报错。
    pub fn kind_filter(&self) -> Result<Vec<AppKind>, String> {
        self.kinds
            .iter()
            .map(|raw| parse_kind(raw))
            .collect::<Result<Vec<_>, _>>()
    }

    pub fn min_size_bytes(&self) -> Result<Option<u64>, String> {
        self.min_size
            .as_deref()
            .map(parse_size)
            .transpose()
    }
}

pub fn parse_kind(raw: &str) -> Result<AppKind, String> {
    let wanted = raw.trim().to_ascii_lowercase();
    for kind in [
        AppKind::Electron,
        AppKind::Edge,
        AppKind::Chrome,
        AppKind::Nwjs,
        AppKind::CefSharp,
        AppKind::MiniElectron,
        AppKind::MiniBlink,
        AppKind::Cef,
        AppKind::Unknown,
    ] {
        if kind.label() == wanted {
            return Ok(kind);
        }
    }
    Err(format!(
        "unknown kind `{raw}` (expected one of: electron, edge, chrome, nwjs, cefsharp, mini_electron, mini_blink, cef, unknown)"
    ))
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
        "" => 1.0,
        "b" => 1.0,
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
}
