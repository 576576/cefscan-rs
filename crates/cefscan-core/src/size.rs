//! 磁盘占用统计。
//!
//! 各应用的根目录互不相关，是天然的并行任务，直接交给 rayon。

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct FileId {
    device: u64,
    inode: u64,
}

/// 累加一个目录树下的文件大小。
///
/// 用显式栈而不是递归：目录深度不可控，递归有爆栈风险。
pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut seen = HashSet::new();

    while let Some(current) = pending.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };

            // 硬链接会让同一个文件被重复计数（Windows 的 WinSxS 尤其明显）。
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                use std::os::unix::fs::MetadataExt;
                if !seen.insert(FileId {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                }) {
                    continue;
                }
            }

            total = total.saturating_add(metadata.len());
            let is_dir = metadata.file_type().is_dir();
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let is_symlink = metadata.file_type().is_symlink();
            #[cfg(target_os = "windows")]
            let is_symlink = false;
            if is_dir && !is_symlink {
                pending.push(entry.path());
            }
        }
    }
    total
}

/// 并行统计一批目录，**每算完一个就回调一次**，不等整批结束。
///
/// 回调顺序不保证（取决于哪个目录先算完），调用方需要确定性顺序就自己排序。
/// 之所以要边算边回调：应用根目录之间大小差异极大，小的几十 MB、大的几个 GB，
/// 等最慢的那个算完再一次性返回，用户会盯着空列表看很久。
///
/// 回调签名里的 `index` 是 `paths` 中的下标，方便调用方回查原始数据。
pub fn sizes_parallel_each<F>(paths: &[PathBuf], threads: usize, on_size: F)
where
    F: Fn(usize, &Path, u64) + Send + Sync,
{
    use rayon::prelude::*;

    let run = || {
        paths
            .par_iter()
            .enumerate()
            .for_each(|(index, path)| on_size(index, path, dir_size(path)));
    };

    if threads <= 1 || paths.len() <= 1 {
        run();
        return;
    }
    match rayon::ThreadPoolBuilder::new()
        .num_threads(threads.min(paths.len()))
        .build()
    {
        Ok(pool) => pool.install(run),
        Err(_) => run(),
    }
}

/// 并行统计一批目录，按输入顺序返回大小。
pub fn sizes_parallel(paths: &[PathBuf], threads: usize) -> Vec<u64> {
    let sizes = std::sync::Mutex::new(vec![0_u64; paths.len()]);
    sizes_parallel_each(paths, threads, |index, _, size| {
        sizes.lock().expect("size slot poisoned")[index] = size;
    });
    sizes.into_inner().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sizes_add_up_recursively() {
        let root = std::env::temp_dir().join(format!("cefscan-size-{}", std::process::id()));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("a.bin"), vec![0_u8; 100]).unwrap();
        fs::write(root.join("nested").join("b.bin"), vec![0_u8; 250]).unwrap();

        assert_eq!(dir_size(&root), 350);
        let parallel = sizes_parallel(&[root.clone()], 4);
        assert_eq!(parallel, vec![350]);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unreadable_directories_do_not_panic() {
        assert_eq!(dir_size(Path::new("/definitely/not/here")), 0);
    }
}
