//! 遍历剪枝规则。
//!
//! 剪枝必须在**目录层**生效：一旦在目录入口就把 `node_modules`、`WinSxS`
//! 这类子树砍掉，后面根本不会再产生读取它的系统调用，这才是省时间的做法。

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::model::ScanOptions;

/// 平台相关的系统目录，扫描它们既慢又无意义。
#[cfg(target_os = "windows")]
pub const PLATFORM_EXCLUDED_DIRS: &[&str] = &["winsxs", "servicing", "recovery"];

#[cfg(target_os = "linux")]
pub const PLATFORM_EXCLUDED_ROOTS: &[&str] =
    &["/proc", "/sys", "/dev", "/run", "/tmp", "/boot", "/lost+found"];

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
    ///
    /// 遍历阶段用它筛目录；索引后端用它筛候选文件——走索引时没有遍历过程可剪枝，
    /// 只能拿到结果后按同一套规则再筛一遍，才能保证两个后端口径一致。
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
        if is_platform_excluded(path) {
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

    fn in_roots(&self, path: &Path) -> bool {
        self.roots
            .is_empty()
            .then_some(true)
            .unwrap_or_else(|| self.roots.iter().any(|root| path_starts_with(path, root)))
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
    path.components().any(|c| component_is_hidden(c.as_os_str()))
}

fn component_is_hidden(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
        && name.as_encoded_bytes().len() > 1
}

fn is_trash_dir(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|text| TRASH_DIRS.iter().any(|t| text.eq_ignore_ascii_case(t)))
}

/// 大小写不敏感、且以目录边界为准的前缀判断。
///
/// `C:\Windows\WinSxS` 必须命中，但 `C:\Windows\WinSxSBackup` 不能命中——
/// 参考实现里就有过这种"前缀误伤"的坑。
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

#[cfg(target_os = "windows")]
fn is_platform_excluded(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| PLATFORM_EXCLUDED_DIRS.contains(&name.to_ascii_lowercase().as_str()))
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_platform_excluded(path: &Path) -> bool {
    let text = path.to_string_lossy();
    PLATFORM_EXCLUDED_ROOTS
        .iter()
        .any(|root| text == *root || text.starts_with(&format!("{root}/")))
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
        // 索引后端拿到的是一条条文件路径，没有遍历过程可剪枝，
        // 必须靠 allows_path 把 roots / 排除规则补上。
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
        // 只在 Windows 有意义，但两条断言在 Unix 上也应成立
        assert!(path_starts_with(Path::new(r"C:\Windows\WinSxS\x"), Path::new(r"C:\Windows\WinSxS")));
        assert!(
            !path_starts_with(Path::new(r"C:\Windows\WinSxSBackup"), Path::new(r"C:\Windows\WinSxS"))
        );
    }

    #[test]
    fn prefix_comparison_ignores_case_and_slash_direction() {
        assert!(path_starts_with(Path::new(r"c:\WINDOWS\app"), Path::new(r"C:\Windows")));
        assert!(path_starts_with(Path::new("C:/Windows/app"), Path::new(r"C:\Windows")));
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
}
