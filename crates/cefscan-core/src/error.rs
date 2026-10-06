//! 错误类型。

use std::io;
use std::path::PathBuf;

/// 扫描过程中可能出现的错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScanError {
    /// 没有任何可遍历的根目录。
    #[error("no filesystem roots are available to scan")]
    NoRoots,

    /// 索引后端与文件系统后端都失败了。
    #[error("index backend failed ({index}); filesystem fallback failed ({fallback})")]
    BothBackendsFailed { index: String, fallback: String },

    /// 构建并行线程池失败。
    #[error("failed to build the thread pool: {0}")]
    ThreadPool(String),

    /// 读取某个路径失败。
    #[error("failed to read {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

impl ScanError {
    #[must_use]
    pub fn io(path: PathBuf, source: io::Error) -> Self {
        Self::Io { path, source }
    }
}

/// 扫描结果：`Ok` 携带统计，单个条目失败只降级不中断。
pub type ScanResult<T> = Result<T, ScanError>;
