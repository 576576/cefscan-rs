//! `cefscan` 的扫描引擎。
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
pub mod naming;
pub mod process;
mod scan;
pub mod signature;
pub mod size;
pub mod walk;

pub use error::{ScanError, ScanResult};
pub use model::{
    AppInfo, AppKind, Backend, Candidate, CandidateKind, Direction, FILESYSTEM_BACKEND, ScanNotice,
    ScanOptions, ScanStats, SortKey,
};
pub use naming::display_name;
pub use scan::{ScanOutcome, detect_backend, scan, scan_streaming, sort_apps};
