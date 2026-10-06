//! 目录检查：在一个候选目录里找出"这是什么应用"以及"它的主程序是哪个"。

use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use crate::model::AppKind;
use crate::signature::{Flavor, SignatureScanner};

#[derive(Debug, Clone, Default)]
pub struct DirInspection {
    pub kind: Option<AppKind>,
    /// 推断出的主程序（可能为 `None`，此时只能以目录形式展示）。
    pub executable: Option<PathBuf>,
    pub evidence: Option<&'static str>,
}

/// 单个条目的判定结果。
enum Finding {
    /// 这条不构成线索。
    Skip,
    /// 记下这条线索，继续看后面的条目。
    Merge {
        found: Option<(AppKind, &'static str)>,
        path: PathBuf,
        launchable: bool,
    },
    /// 文件名直接定案（Edge / Chrome），后面的条目不用看了。
    Decided(DirInspection),
}

/// 累加出来的"这个目录里最好的线索"。
#[derive(Default)]
struct Best {
    /// 最强签名命中。
    signature: Option<(AppKind, &'static str)>,
    /// 该命中所属的文件。
    path: Option<PathBuf>,
    /// 该文件是否可启动。
    launchable: bool,
    /// 兜底：没命中签名时，分最高的可启动文件。
    fallback: Option<(u8, PathBuf)>,
}

impl Best {
    fn merge(
        &mut self,
        dir: &Path,
        found: Option<(AppKind, &'static str)>,
        path: PathBuf,
        launchable: bool,
    ) {
        if launchable {
            let score = executable_score(&path, dir);
            if self
                .fallback
                .as_ref()
                .is_none_or(|(best_score, _)| score > *best_score)
            {
                self.fallback = Some((score, path.clone()));
            }
        }

        if let Some((kind, needle)) = found
            && self
                .signature
                .is_none_or(|(current, _)| kind.rank() > current.rank())
        {
            self.signature = Some((kind, needle));
            self.path = Some(path);
            self.launchable = launchable;
        }
    }

    fn into_inspection(self) -> DirInspection {
        DirInspection {
            kind: self.signature.map(|(kind, _)| kind),
            executable: if self.launchable {
                self.path
            } else {
                self.fallback.map(|(_, path)| path)
            },
            evidence: self.signature.map(|(_, needle)| needle),
        }
    }
}

/// 检查单个目录。`scanner` 由调用方按任务持有并复用（含内部缓冲）。
///
/// 用 `try_fold` + `ControlFlow` 表达"逐条累加，遇到文件名定案就短路"，
/// 取代原来 4 个可变局部 + 提前 `return` 的写法。
pub fn inspect_directory(
    dir: &Path,
    flavor: Flavor,
    scanner: &mut SignatureScanner,
) -> DirInspection {
    let Ok(entries) = fs::read_dir(dir) else {
        return DirInspection::default();
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    // `sort_by_key` 每次比较都会调两次 key 函数（`entry.path()` 各分配一个 PathBuf）；
    // `sort_by_cached_key` 每个条目只取一次 key。实测 n = 64 时 126 次 → 64 次。
    entries.sort_by_cached_key(std::fs::DirEntry::path);

    let outcome: ControlFlow<DirInspection, Best> =
        entries
            .into_iter()
            .try_fold(Best::default(), |mut best, entry| {
                match classify_entry(&entry, flavor, scanner) {
                    Finding::Skip => {}
                    Finding::Merge {
                        found,
                        path,
                        launchable,
                    } => best.merge(dir, found, path, launchable),
                    Finding::Decided(inspection) => return ControlFlow::Break(inspection),
                }
                ControlFlow::Continue(best)
            });

    match outcome {
        ControlFlow::Continue(best) => best.into_inspection(),
        ControlFlow::Break(inspection) => inspection,
    }
}

/// Edge / Chrome 直接靠文件名定案。
fn browser_from_file_name(lowercased: &str) -> Option<AppKind> {
    match lowercased {
        "msedge" | "msedge.exe" | "msedge_proxy.exe" => Some(AppKind::Edge),
        "chrome" | "chrome.exe" => Some(AppKind::Chrome),
        _ => None,
    }
}

/// 判定单个目录条目。只读 `scanner` 的复用缓冲，可单独测。
fn classify_entry(entry: &fs::DirEntry, flavor: Flavor, scanner: &mut SignatureScanner) -> Finding {
    let Ok(metadata) = entry.metadata() else {
        return Finding::Skip;
    };
    if !metadata.is_file() {
        return Finding::Skip;
    }

    let path = entry.path();
    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();

    if matches!(flavor, Flavor::Standard)
        && let Some(kind) = browser_from_file_name(&name)
    {
        return Finding::Decided(DirInspection {
            kind: Some(kind),
            executable: Some(path),
            evidence: Some("filename"),
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let is_executable = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(target_os = "windows")]
    let is_executable = false;

    let is_shared = is_shared_library(&name);
    let is_windows_executable = has_extension(&name, "exe");
    if !is_executable && !is_shared && !is_windows_executable {
        return Finding::Skip;
    }
    if matches!(flavor, Flavor::Mini) && is_shared {
        return Finding::Skip;
    }
    if matches!(flavor, Flavor::Standard) && is_shared && !is_relevant_shared_library(&name) {
        return Finding::Skip;
    }

    let launchable =
        !is_shared && !is_unwanted_executable(&name) && (is_executable || is_windows_executable);

    let Ok(found) = scanner.scan_file(&path, flavor) else {
        return Finding::Skip;
    };

    Finding::Merge {
        found,
        path,
        launchable,
    }
}

/// 文件名是否带某个扩展名（大小写不敏感，不依赖调用方预先转小写）。
fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

pub(crate) fn is_shared_library(name: &str) -> bool {
    has_extension(name, "dll")
        || has_extension(name, "dylib")
        || has_extension(name, "so")
        // `libcef.so.1` 这类带版本号的，扩展名是 `1`，只能按子串认。
        || name.contains(".so.")
}

/// 只对与内核相关的动态库做内容扫描。
pub(crate) fn is_relevant_shared_library(name: &str) -> bool {
    name.contains("cef") || name == "nw.dll" || name.starts_with("libnw.")
}

pub(crate) fn is_unwanted_executable(name: &str) -> bool {
    name.contains("unins")
        || name.contains("setup")
        || name.contains("report")
        || name == "disk-free"
        || name == "chrome-sandbox"
        || name.contains("crashpad_handler")
}

/// 给"哪个可执行文件更像主程序"打分。
pub(crate) fn executable_score(path: &Path, directory: &Path) -> u8 {
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let directory_name = directory
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();

    let mut score: u8 = 10;
    if extension == "exe" || extension == "appimage" {
        score += 40;
    } else if extension.is_empty() {
        score += 30;
    }
    if path
        .file_stem()
        .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(&directory_name))
    {
        score += 20;
    }
    if file_name.contains("web") || file_name.contains("browser") || file_name.contains("cef") {
        score += 30;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 扩展名判定不依赖调用方是否预先转小写，且认得带版本号的 `libcef.so.1`。
    #[test]
    fn shared_libraries_are_recognised_by_extension() {
        assert!(has_extension("Chrome.EXE", "exe"));
        assert!(!has_extension("chrome", "exe"));
        assert!(!has_extension("chrome.exe.bak", "exe"));

        for name in ["libcef.dll", "libcef.so", "libcef.so.1", "libcef.dylib"] {
            assert!(is_shared_library(name), "{name} 应该算共享库");
        }
        for name in ["chrome.exe", "myapp", "notes.txt"] {
            assert!(!is_shared_library(name), "{name} 不该算共享库");
        }
    }

    #[test]
    fn only_framework_libraries_are_worth_scanning() {
        assert!(is_relevant_shared_library("libcef.so"));
        assert!(is_relevant_shared_library("cefsharp.core.dll"));
        assert!(is_relevant_shared_library("libnw.so"));
        assert!(!is_relevant_shared_library("libvulkan.so"));
        assert!(!is_relevant_shared_library("steamclient.dll"));
    }

    #[test]
    fn installer_and_helper_binaries_are_not_candidates() {
        assert!(is_unwanted_executable("unins000.exe"));
        assert!(is_unwanted_executable("setup.exe"));
        assert!(is_unwanted_executable("crashpad_handler.exe"));
        assert!(is_unwanted_executable("chrome-sandbox"));
        assert!(!is_unwanted_executable("myapp.exe"));
    }

    #[test]
    fn executables_matching_their_directory_score_higher() {
        let dir = Path::new("/apps/MyApp");
        let matching = Path::new("/apps/MyApp/MyApp.exe");
        let other = Path::new("/apps/MyApp/helper.exe");
        assert!(executable_score(matching, dir) > executable_score(other, dir));
    }

    #[test]
    fn web_browser_and_cef_names_score_higher() {
        let dir = Path::new("/apps/x");
        assert!(
            executable_score(Path::new("/apps/x/browser.exe"), dir)
                > executable_score(Path::new("/apps/x/tool.exe"), dir)
        );
    }
}
