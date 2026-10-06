//! 目录检查：在一个候选目录里找出"这是什么应用"以及"它的主程序是哪个"。

use std::fs;
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

/// 检查单个目录。`scanner` 由调用方按任务持有并复用（含内部缓冲）。
pub fn inspect_directory(
    dir: &Path,
    flavor: Flavor,
    scanner: &mut SignatureScanner,
) -> DirInspection {
    let Ok(entries) = fs::read_dir(dir) else {
        return DirInspection::default();
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::path);

    let mut best: Option<(AppKind, &'static str)> = None;
    let mut best_path: Option<PathBuf> = None;
    let mut best_launchable = false;
    let mut fallback: Option<(u8, PathBuf)> = None;

    for entry in entries {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }

        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();

        // Edge / Chrome 靠文件名判定。
        if matches!(flavor, Flavor::Standard) {
            let special = match name.as_str() {
                "msedge" | "msedge.exe" | "msedge_proxy.exe" => Some(AppKind::Edge),
                "chrome" | "chrome.exe" => Some(AppKind::Chrome),
                _ => None,
            };
            if let Some(kind) = special {
                return DirInspection {
                    kind: Some(kind),
                    executable: Some(path),
                    evidence: Some("filename"),
                };
            }
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
            continue;
        }
        if matches!(flavor, Flavor::Mini) && is_shared {
            continue;
        }
        if matches!(flavor, Flavor::Standard) && is_shared && !is_relevant_shared_library(&name) {
            continue;
        }

        let launchable = !is_shared
            && !is_unwanted_executable(&name)
            && (is_executable || is_windows_executable);

        let Ok(found) = scanner.scan_file(&path, flavor) else {
            continue;
        };

        if launchable {
            let score = executable_score(&path, dir);
            if fallback
                .as_ref()
                .is_none_or(|(best_score, _)| score > *best_score)
            {
                fallback = Some((score, path.clone()));
            }
        }

        if let Some((kind, needle)) = found
            && best.is_none_or(|(current, _)| kind.rank() > current.rank())
        {
            best = Some((kind, needle));
            best_path = Some(path);
            best_launchable = launchable;
        }
    }

    let executable = if best_launchable {
        best_path
    } else {
        fallback.map(|(_, path)| path)
    };
    DirInspection {
        kind: best.map(|(kind, _)| kind),
        executable,
        evidence: best.map(|(_, needle)| needle),
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
