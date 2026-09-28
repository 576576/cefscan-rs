//! 从路径推导一个给人看的应用名。
//!
//! 扫描结果里的 `path` 是"最能代表这个应用的那个文件或目录"，直接拿来当名字
//! 通常没法看：`...\Microsoft VS Code\Code.exe` 会显示成 "Code"，
//! `...\Edge\Application\154.0.4258.37\msedge.exe` 会显示成 "msedge"。
//!
//! 所以这里从路径所在目录往上走，跳过版本号目录（`154.0.4258.37`、`app-3.6.6`）
//! 和通用目录名（`Application`、`Bin64`、`runtime`），取第一个有意义的名字；
//! 走到用户目录/系统目录这类"边界"就停下，退回文件名本身。
//!
//! 纯字符串逻辑，不碰文件系统，所以好测。

use std::ffi::OsStr;
use std::path::Path;

/// 往上看多少层就放弃。
const MAX_DEPTH: usize = 6;

/// 通用目录名：夹在应用名和根目录之间，但不是应用名本身。
const GENERIC_SEGMENTS: &[&str] = &[
    "addons",
    "amd64",
    "app",
    "application",
    "applications",
    "apps",
    "bin",
    "bin64",
    "binaries",
    "build",
    "client",
    "content",
    "contents",
    "current",
    "dist",
    "extracted",
    "framework",
    "lib",
    "libs",
    "out",
    "output",
    "plugins",
    "release",
    "resources",
    "runtime",
    "src",
    "target",
    "version",
    "versions",
    "win32",
    "win64",
    "windows",
    "x64",
    "x86",
];

/// 走到这些名字就停：再往上就是用户目录或系统目录，取到的名字没有意义。
const STOP_SEGMENTS: &[&str] = &[
    "appdata",
    "application data",
    "common",
    "desktop",
    "documents",
    "downloads",
    "local",
    "localappdata",
    "program files",
    "program files (x86)",
    "programdata",
    "programs",
    "roaming",
    "steamapps",
    "system32",
    "users",
    "windows",
];

/// 推导展示名。永远返回非空字符串。
pub fn display_name(path: &Path) -> String {
    if let Some(directory) = path.parent() {
        for ancestor in directory.ancestors().take(MAX_DEPTH) {
            let Some(raw) = ancestor.file_name().and_then(OsStr::to_str) else {
                continue; // 盘符根（`C:\`）没有 file_name
            };
            let name = strip_package_suffix(raw);
            if name.is_empty() {
                continue;
            }
            if is_stop_segment(name) {
                break; // 再往上没有意义了，直接退回文件名
            }
            if is_generic(name) || is_version_like(name) {
                continue;
            }
            return name.to_owned();
        }
    }

    if let Some(stem) = path.file_stem().and_then(OsStr::to_str)
        && !stem.is_empty()
    {
        return strip_package_suffix(stem).to_owned();
    }

    // 走到这里说明路径本身就没有可用成分（空路径之类）。名字列留空会显得像坏了，
    // 所以给一个占位。
    let rendered = path.display().to_string();
    if rendered.is_empty() {
        "(未知)".to_owned()
    } else {
        rendered
    }
}

/// 去掉 WindowsApps 那种包目录名的后缀：
/// `Crystalnix.Termius_10.1.0.0_x64__0m0t0j9spf6x8` → `Crystalnix.Termius`。
fn strip_package_suffix(name: &str) -> &str {
    let bytes = name.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'_' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
            return &name[..index];
        }
    }
    name
}

fn is_generic(name: &str) -> bool {
    GENERIC_SEGMENTS
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn is_stop_segment(name: &str) -> bool {
    STOP_SEGMENTS
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// 形如 `154.0.4258.37`、`app-3.6.6`、`office6`、`11581` 的版本号目录。
///
/// 判据：把开头的字母和分隔符剥掉之后，剩下的是"纯数字 + 点/横线/下划线"，
/// 且至少含一个数字。这样 `BeamNG.drive`（剥完是空）和 `360se6`（含字母）
/// 都不会被误判成版本号。
fn is_version_like(name: &str) -> bool {
    let rest = name
        .trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '-' || c == '_' || c == '.');
    if rest.is_empty() {
        return false;
    }
    rest.chars().any(|c| c.is_ascii_digit())
        && rest
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '_' | ' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全部取自本机真实扫描结果，改启发式时先看这张表。
    #[test]
    fn real_world_paths_get_readable_names() {
        let cases = [
            (
                r"C:\Users\16695\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe",
                "WorkBuddy",
            ),
            (
                r"C:\Users\16695\AppData\Local\Programs\Microsoft VS Code\Code.exe",
                "Microsoft VS Code",
            ),
            (
                r"C:\Users\16695\AppData\Local\GitHubDesktop\app-3.6.6\GitHubDesktop.exe",
                "GitHubDesktop",
            ),
            (
                r"C:\Program Files (x86)\Microsoft\Edge\Application\154.0.4258.37\msedge.exe",
                "Edge",
            ),
            (
                r"C:\Users\16695\AppData\Local\PD Launcher\app-1.8.0.5559\crashagent64.exe",
                "PD Launcher",
            ),
            (
                r"C:\Program Files\Tencent\WeMeet\3.46.11.413\CrashpadHandlerExtension.exe",
                "WeMeet",
            ),
            (
                r"C:\Program Files\Tencent\QQNT\versions\9.9.33-52230",
                "QQNT",
            ),
            (
                r"D:\Program Files\Steam\steamapps\common\Hearts of Iron IV\dowser.exe",
                "Hearts of Iron IV",
            ),
            (
                r"D:\Program Files\Steam\steamapps\common\BeamNG.drive\Bin64\BeamNG.drive.x64.exe",
                "BeamNG.drive",
            ),
            (
                r"D:\Program Files\Tencent\Androws\Application\5.10.3700.4812\CefRendererProcess.exe",
                "Androws",
            ),
            (
                r"C:\Program Files\WindowsApps\Crystalnix.Termius_10.1.0.0_x64__0m0t0j9spf6x8\app\Termius.exe",
                "Crystalnix.Termius",
            ),
        ];

        for (path, expected) in cases {
            assert_eq!(display_name(Path::new(path)), expected, "路径 {path}");
        }
    }

    #[test]
    fn stops_at_user_and_system_directories() {
        // 直接躺在 Programs 下面的 exe 没有更有意义的目录名可用，
        // 应该退回文件名而不是显示 "Local" 或 "Programs"。
        assert_eq!(
            display_name(Path::new(r"C:\Users\me\AppData\Local\Programs\tool.exe")),
            "tool"
        );
        assert_eq!(display_name(Path::new(r"C:\Program Files\app.exe")), "app");
    }

    #[test]
    fn falls_back_to_file_stem() {
        assert_eq!(display_name(Path::new(r"C:\solo.exe")), "solo");
        assert_eq!(display_name(Path::new("libcef.dll")), "libcef");
    }

    #[test]
    fn never_returns_empty() {
        for path in [r"C:\", "/", "", "x"] {
            assert!(!display_name(Path::new(path)).is_empty(), "路径 {path:?}");
        }
    }

    #[test]
    fn version_like_detection_is_not_greedy() {
        assert!(is_version_like("154.0.4258.37"));
        assert!(is_version_like("app-3.6.6"));
        assert!(is_version_like("office6"));
        assert!(is_version_like("11581"));
        assert!(is_version_like("v1.2.3"));

        // 这些是真的应用名，不能被当成版本号
        assert!(!is_version_like("BeamNG.drive"));
        assert!(!is_version_like("360se6"));
        assert!(!is_version_like("Code"));
        assert!(!is_version_like("7-Zip"));
        assert!(!is_version_like("Microsoft VS Code"));
    }

    #[test]
    fn package_suffix_is_stripped() {
        assert_eq!(
            strip_package_suffix("Crystalnix.Termius_10.1.0.0_x64__0m0t0j9spf6x8"),
            "Crystalnix.Termius"
        );
        assert_eq!(strip_package_suffix("WorkBuddy"), "WorkBuddy");
        assert_eq!(strip_package_suffix("foo_bar"), "foo_bar"); // 后面不是数字
    }
}
