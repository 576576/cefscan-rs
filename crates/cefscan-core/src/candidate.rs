//! 候选文件名的判定。

#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::borrow::Cow;
use std::ffi::OsStr;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::ffi::OsString;

use crate::model::CandidateKind;

/// 判定一个文件名是否值得进一步检查。
///
/// 在 Windows / macOS 上按 ASCII 小写比较（文件系统大小写不敏感），
/// 在 Linux 上大小写敏感。
pub fn classify_candidate_name(name: &OsStr) -> Option<CandidateKind> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let lowered: Cow<'_, OsStr> = match name.to_str() {
        Some(text) => Cow::Owned(OsString::from(text.to_ascii_lowercase())),
        None => Cow::Borrowed(name),
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let name = lowered.as_encoded_bytes();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let name = name.as_encoded_bytes();

    // `chrome_100_percent.pak`：Chromium 系通用的资源包。
    if contains_bytes(name, b"_100_") && name.ends_with(b".pak") {
        return Some(CandidateKind::Pak);
    }

    if matches_cef_name(name) {
        return Some(CandidateKind::Cef);
    }
    if matches_node_name(name) {
        return Some(CandidateKind::Node);
    }
    None
}

fn matches_cef_name(name: &[u8]) -> bool {
    matches!(name, b"libcef.so" | b"libcef.dll" | b"libcef.dylib")
        || name.starts_with(b"libcef.so.")
        || matches!(name, b"chromium embedded framework" | b"electron framework")
}

fn matches_node_name(name: &[u8]) -> bool {
    matches!(name, b"libnode.so" | b"libnode.dll" | b"libnode.dylib")
        || name.starts_with(b"libnode.so.")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(haystack, needle).is_some()
}

#[cfg(test)]
mod tests {
    use super::classify_candidate_name;
    use crate::model::CandidateKind;
    use std::ffi::OsStr;

    fn class(name: &str) -> Option<CandidateKind> {
        classify_candidate_name(OsStr::new(name))
    }

    #[test]
    fn pak_candidates_need_the_percent_marker() {
        assert_eq!(class("chrome_100_percent.pak"), Some(CandidateKind::Pak));
        assert_eq!(class("chrome_200_percent.pak"), None);
        assert_eq!(class("resources.pak"), None);
    }

    #[test]
    fn cef_candidates_cover_every_platform_spelling() {
        for name in ["libcef.so", "libcef.so.123", "libcef.dll", "libcef.dylib"] {
            assert_eq!(class(name), Some(CandidateKind::Cef), "{name}");
        }
    }

    #[test]
    fn node_candidates_cover_every_platform_spelling() {
        for name in [
            "libnode.so",
            "libnode.so.115",
            "libnode.dll",
            "libnode.dylib",
        ] {
            assert_eq!(class(name), Some(CandidateKind::Node), "{name}");
        }
    }

    #[test]
    fn similar_names_are_rejected() {
        for name in [
            "libcefdetector.rmeta",
            "libnode_helpers.so",
            "libcefextra.dll",
            "cef.exe",
        ] {
            assert_eq!(class(name), None, "{name} should not match");
        }
    }

    #[test]
    fn framework_names_are_recognised() {
        assert_eq!(
            class("chromium embedded framework"),
            Some(CandidateKind::Cef)
        );

        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(
                class("Chromium Embedded Framework"),
                Some(CandidateKind::Cef)
            );
            assert_eq!(class("Electron Framework"), Some(CandidateKind::Cef));
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn framework_names_are_case_insensitive() {
        assert_eq!(class("LIBCEF.DLL"), Some(CandidateKind::Cef));
        assert_eq!(class("ELECTRON FRAMEWORK"), Some(CandidateKind::Cef));
    }

    /// Linux 上大小写敏感：带大写的拼写不该命中，全小写的才命中。
    #[cfg(target_os = "linux")]
    #[test]
    fn names_are_case_sensitive_on_linux() {
        assert_eq!(class("LIBCEF.DLL"), None);
        assert_eq!(class("libcef.dll"), Some(CandidateKind::Cef));
    }
}
