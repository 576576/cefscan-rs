//! 遍历剪枝规则。

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::model::ScanOptions;

/// 平台相关的系统目录/根。
#[cfg(target_os = "windows")]
pub const PLATFORM_EXCLUDED_DIRS: &[&str] = &["winsxs", "servicing", "recovery"];

#[cfg(target_os = "linux")]
pub const PLATFORM_EXCLUDED_ROOTS: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/tmp",
    "/boot",
    "/lost+found",
];

#[cfg(target_os = "macos")]
pub const PLATFORM_EXCLUDED_ROOTS: &[&str] = &["/dev", "/System/Volumes/Data", "/private/var/db"];

/// 回收站目录名。
const TRASH_DIRS: &[&str] = &[".trash", "trash", ".trashes", "$recycle.bin"];

/// 遍历剪枝器。
#[derive(Debug, Clone)]
pub struct Filter {
    roots: Vec<PathBuf>,
    exclude_dir_names: Vec<String>,
    exclude_paths: Vec<PathBuf>,
    include_hidden: bool,
    follow_symlinks: bool,
}

impl Filter {
    pub fn new(options: &ScanOptions) -> Self {
        Self {
            roots: options.roots.clone(),
            exclude_dir_names: options.exclude_dir_names.clone(),
            exclude_paths: options.exclude_paths.clone(),
            include_hidden: options.include_hidden,
            follow_symlinks: options.follow_symlinks,
        }
    }

    /// 一个路径（目录**或文件**）是否通过全部规则。
    pub fn allows_path(&self, path: &Path) -> bool {
        if !self.in_roots(path) {
            return false;
        }
        if self
            .exclude_paths
            .iter()
            .any(|excluded| path_starts_with(path, excluded))
        {
            return false;
        }
        if !self.include_hidden && is_hidden(path) {
            return false;
        }
        if self.is_platform_excluded(path) {
            return false;
        }
        for component in path.components() {
            let name = component.as_os_str();
            if !self.include_hidden && component_is_hidden(name) {
                return false;
            }
            if self.is_excluded_dir_name(name) || is_trash_dir(name) {
                return false;
            }
        }
        true
    }

    /// 一个目录是否值得进入。返回 `false` 时整棵子树都被跳过。
    pub fn allows_dir(&self, path: &Path) -> bool {
        self.allows_path(path)
    }

    /// 是否命中平台自己的排除规则。
    ///
    /// 两个平台的规则形状不同：Windows 是「按目录名」，命中任何一层都排除；
    /// Unix 是「按根」，只在路径落在这些根之下时排除。
    #[cfg(target_os = "windows")]
    fn is_platform_excluded(&self, path: &Path) -> bool {
        path.components().any(|component| {
            component.as_os_str().to_str().is_some_and(|name| {
                PLATFORM_EXCLUDED_DIRS.contains(&name.to_ascii_lowercase().as_str())
            })
        })
    }

    /// 是否命中平台排除的根（Unix：按根）。
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn is_platform_excluded(&self, path: &Path) -> bool {
        let text = path.to_string_lossy();
        PLATFORM_EXCLUDED_ROOTS
            .iter()
            .any(|root| excluded_root_hit(&text, root, &self.roots))
    }

    fn in_roots(&self, path: &Path) -> bool {
        if self.roots.is_empty() {
            true
        } else {
            self.roots.iter().any(|root| path_starts_with(path, root))
        }
    }

    fn is_excluded_dir_name(&self, name: &OsStr) -> bool {
        let text = match name.to_str() {
            Some(text) => text,
            None => return false,
        };
        self.exclude_dir_names
            .iter()
            .any(|excluded| text.eq_ignore_ascii_case(excluded))
    }

    /// 记录里是否跟随符号链接。
    pub fn follow_symlinks(&self) -> bool {
        self.follow_symlinks
    }
}

fn is_hidden(path: &Path) -> bool {
    path.components()
        .any(|c| component_is_hidden(c.as_os_str()))
}

fn component_is_hidden(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.') && name.as_encoded_bytes().len() > 1
}

fn is_trash_dir(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|text| TRASH_DIRS.iter().any(|t| text.eq_ignore_ascii_case(t)))
}

/// 大小写不敏感、且以目录边界为准的前缀判断。
pub fn path_starts_with(path: &Path, root: &Path) -> bool {
    let path = path.as_os_str().as_encoded_bytes();
    let root = root.as_os_str().as_encoded_bytes();
    if path.len() < root.len() {
        return false;
    }
    for (index, expected) in root.iter().enumerate() {
        if ascii_fold(path[index]) != ascii_fold(*expected) {
            return false;
        }
    }
    if matches!(root.last(), Some(b'/') | Some(b'\\')) {
        return true;
    }
    path.get(root.len())
        .is_none_or(|byte| *byte == b'/' || *byte == b'\\')
}

fn ascii_fold(byte: u8) -> u8 {
    if byte.is_ascii_uppercase() {
        byte.to_ascii_lowercase()
    } else if byte == b'/' {
        b'\\'
    } else {
        byte
    }
}

/// `path` 是否落在 `excluded_root` 之下、且这个根没有被显式 root 覆盖。
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn excluded_root_hit(path: &str, excluded_root: &str, explicit_roots: &[PathBuf]) -> bool {
    let inside = path == excluded_root || path.starts_with(&format!("{excluded_root}/"));
    inside
        && !explicit_roots
            .iter()
            .any(|explicit| path_starts_with(explicit, Path::new(excluded_root)))
}

/// 允许在测试里复用的去重集合。
pub fn dir_name_set(names: &[String]) -> HashSet<String> {
    names.iter().map(|n| n.to_ascii_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ScanOptions;

    fn filter_with(roots: Vec<PathBuf>, exclude: &[&str]) -> Filter {
        Filter::new(&ScanOptions {
            roots,
            exclude_dir_names: exclude.iter().map(|s| s.to_string()).collect(),
            ..ScanOptions::default()
        })
    }

    #[test]
    fn excluded_directory_names_prune_whole_subtrees() {
        let filter = filter_with(vec![PathBuf::from("/allowed")], &["node_modules", "target"]);
        assert!(filter.allows_dir(Path::new("/allowed/app")));
        assert!(!filter.allows_dir(Path::new("/allowed/node_modules")));
        assert!(!filter.allows_dir(Path::new("/allowed/target/debug")));
    }

    #[test]
    fn configured_roots_limit_the_scan_scope() {
        let filter = filter_with(vec![PathBuf::from("/allowed")], &[]);
        assert!(filter.allows_dir(Path::new("/allowed/app")));
        assert!(!filter.allows_dir(Path::new("/other/app")));
    }

    #[test]
    fn file_paths_are_filtered_by_the_same_rules() {
        let filter = filter_with(vec![PathBuf::from("/allowed")], &["node_modules"]);
        assert!(filter.allows_path(Path::new("/allowed/app/libcef.dll")));
        assert!(!filter.allows_path(Path::new("/other/app/libcef.dll")));
        assert!(!filter.allows_path(Path::new("/allowed/node_modules/x/libcef.dll")));
        assert!(!filter.allows_path(Path::new("/allowed/.cache/libcef.dll")));
    }

    #[test]
    fn empty_roots_allow_everything() {
        let filter = filter_with(Vec::new(), &[]);
        assert!(filter.allows_path(Path::new("/anywhere/libcef.dll")));
    }

    #[test]
    fn trailing_names_must_not_be_prefix_matched() {
        assert!(path_starts_with(
            Path::new(r"C:\Windows\WinSxS\x"),
            Path::new(r"C:\Windows\WinSxS")
        ));
        assert!(!path_starts_with(
            Path::new(r"C:\Windows\WinSxSBackup"),
            Path::new(r"C:\Windows\WinSxS")
        ));
    }

    #[test]
    fn prefix_comparison_ignores_case_and_slash_direction() {
        assert!(path_starts_with(
            Path::new(r"c:\WINDOWS\app"),
            Path::new(r"C:\Windows")
        ));
        assert!(path_starts_with(
            Path::new("C:/Windows/app"),
            Path::new(r"C:\Windows")
        ));
    }

    #[test]
    fn hidden_directories_are_skipped_by_default() {
        let filter = filter_with(vec![PathBuf::from("/allowed")], &[]);
        assert!(!filter.allows_dir(Path::new("/allowed/.cache")));
        let mut options = ScanOptions {
            roots: vec![PathBuf::from("/allowed")],
            include_hidden: true,
            ..ScanOptions::default()
        };
        assert!(Filter::new(&options).allows_dir(Path::new("/allowed/.cache")));
        options.include_hidden = false;
    }

    #[test]
    fn trash_directories_are_always_skipped() {
        let filter = filter_with(vec![PathBuf::from("/allowed")], &[]);
        assert!(!filter.allows_dir(Path::new("/allowed/.Trash/1")));
    }

    #[test]
    fn roots_ending_with_a_separator_still_contain_their_subtree() {
        assert!(path_starts_with(
            Path::new(r"C:\Windows\System32"),
            Path::new(r"C:\")
        ));
        assert!(path_starts_with(Path::new(r"C:\"), Path::new(r"C:\")));
        assert!(path_starts_with(Path::new("/usr/lib"), Path::new("/")));
        assert!(path_starts_with(Path::new("/"), Path::new("/")));
    }

    #[test]
    fn excluded_roots_are_overridden_by_explicit_roots() {
        let no_explicit: Vec<PathBuf> = Vec::new();

        // 默认全盘扫描（没有显式 root）：名单生效。
        assert!(excluded_root_hit("/tmp/foo/app", "/tmp", &no_explicit));
        assert!(excluded_root_hit("/proc/1/fd", "/proc", &no_explicit));
        assert!(excluded_root_hit("/tmp", "/tmp", &no_explicit)); // 根本身也算命中
        // 名字相近但不是子树，不能误伤。
        assert!(!excluded_root_hit("/tmpfoo/app", "/tmp", &no_explicit));
        // 名单之外的路径不受影响。
        assert!(!excluded_root_hit(
            "/usr/lib/electron",
            "/tmp",
            &no_explicit
        ));

        // 用户点名扫 /tmp/foo：/tmp 这一条不再生效。
        let explicit = vec![PathBuf::from("/tmp/foo")];
        assert!(!excluded_root_hit("/tmp/foo/app", "/tmp", &explicit));
        assert!(!excluded_root_hit("/tmp/other", "/tmp", &explicit)); // 由 roots 把关，不在这里判

        // 显式给 `/` 等于默认全盘（`/` 不是 /tmp 的祖先），名单照旧生效。
        let root = vec![PathBuf::from("/")];
        assert!(excluded_root_hit("/tmp/foo", "/tmp", &root));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_system_directories_are_pruned() {
        let filter = filter_with(vec![PathBuf::from(r"C:\")], &[]);
        assert!(!filter.allows_dir(Path::new(r"C:\Windows\WinSxS\amd64_x86")));
        assert!(filter.allows_dir(Path::new(r"C:\Windows\System32")));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn explicit_roots_win_over_platform_excluded_roots() {
        let filter = filter_with(vec![PathBuf::from("/tmp/foo")], &[]);
        assert!(filter.allows_dir(Path::new("/tmp/foo/app")));
        assert!(filter.allows_path(Path::new("/tmp/foo/app/libcef.so")));
        // roots 之外的地方照旧进不去。
        assert!(!filter.allows_dir(Path::new("/tmp/other/app")));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn platform_excluded_roots_still_prune_default_scans() {
        // 没有显式 root（默认全盘）时名单照旧生效。
        let filter = filter_with(Vec::new(), &[]);
        assert!(!filter.allows_dir(Path::new("/tmp/foo/app")));
        assert!(!filter.allows_dir(Path::new("/proc/1/fd")));
        assert!(filter.allows_dir(Path::new("/usr/lib/electron")));

        // 显式给 `/` 等于默认全盘，名单同样生效（`/` 并不是 /tmp 的祖先）。
        let root_filter = filter_with(vec![PathBuf::from("/")], &[]);
        assert!(!root_filter.allows_dir(Path::new("/tmp/foo")));
    }
}
