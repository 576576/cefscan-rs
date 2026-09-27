//! `cefscan` 的扫描引擎。
//!
//! 流水线分五段，后段只依赖前段的输出，便于分别替换和测试：
//!
//! 1. [`walk`] —— 自写的并行遍历，产出极少数候选文件
//! 2. [`candidate`] —— 候选文件名判定
//! 3. [`signature`] —— 二进制签名扫描，判定内核类型
//! 4. [`group`] —— 按应用根目录归并去重
//! 5. [`size`] / [`process`] —— 磁盘占用与运行态
//!
//! # Examples
//!
//! ```no_run
//! use cefscan_core::{ScanOptions, scan};
//!
//! let outcome = scan(&ScanOptions::default())?;
//! println!("found {} apps, {} bytes", outcome.apps.len(), outcome.stats.total_bytes);
//! # Ok::<(), cefscan_core::ScanError>(())
//! ```

pub mod candidate;
pub mod error;
pub mod filter;
pub mod group;
pub mod inspect;
pub mod model;
pub mod process;
mod scan;
pub mod signature;
pub mod size;
pub mod walk;

pub use error::{ScanError, ScanResult};
pub use model::{AppInfo, AppKind, Backend, Candidate, CandidateKind, ScanOptions, ScanStats};
pub use scan::{ScanOutcome, scan, scan_streaming, sort_apps};
