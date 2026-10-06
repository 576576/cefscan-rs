//! 运行中进程检测。

use std::collections::HashSet;
use std::path::Path;

#[cfg(target_os = "windows")]
pub type ProcessKey = String;

#[cfg(not(target_os = "windows"))]
pub type ProcessKey = std::path::PathBuf;

#[cfg(target_os = "windows")]
#[must_use]
pub fn running_processes() -> HashSet<ProcessKey> {
    use std::ffi::OsString;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: OwnedHandle 只由有效的、拥有所有权的句柄构造。
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    // SAFETY: 无借用参数，返回的句柄由 OwnedHandle 负责关闭。
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return HashSet::new();
    }
    let snapshot = OwnedHandle(snapshot);

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut processes = HashSet::new();

    // SAFETY: entry 已填好 dwSize，且在枚举期间保持有效。
    if unsafe { Process32FirstW(snapshot.0, &raw mut entry) } == 0 {
        return processes;
    }

    // 路径缓冲只分配一次：进程数以百计，逐进程 `vec![0; 64 KiB]` 是几十 MB 的无谓开销。
    let mut path_buffer = vec![0_u16; 32_768];

    loop {
        // SAFETY: OpenProcess 借用进程号；句柄非空时由 OwnedHandle 关闭。
        let process =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID) };
        if !process.is_null() {
            let process = OwnedHandle(process);
            let mut length = path_buffer.len() as u32;
            // SAFETY: path_buffer 可写 length 个 UTF-16 单元；受保护进程会失败，忽略即可。
            if unsafe {
                QueryFullProcessImageNameW(process.0, 0, path_buffer.as_mut_ptr(), &raw mut length)
            } != 0
            {
                let path =
                    std::path::PathBuf::from(OsString::from_wide(&path_buffer[..length as usize]));
                processes.insert(normalize_windows_path(&path));
            }
        }

        // SAFETY: entry 仍带正确的 dwSize。
        if unsafe { Process32NextW(snapshot.0, &raw mut entry) } == 0 {
            break;
        }
    }

    processes
}

/// `\\?\UNC\Server\Share\x` → `\\server\share\x`；`\\?\C:\a/b.exe` → `c:\a\b.exe`。
#[cfg(target_os = "windows")]
#[must_use]
pub fn normalize_windows_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    if text.len() >= 8 && text[..8].eq_ignore_ascii_case(r"\\?\UNC\") {
        format!(r"\\{}", &text[8..])
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    }
    .to_lowercase()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn running_processes() -> HashSet<ProcessKey> {
    let mut processes = HashSet::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return processes;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            && let Ok(target) = std::fs::read_link(entry.path().join("exe"))
        {
            processes.insert(target);
        }
    }
    processes
}

/// 判断某个可执行文件是否正在运行。
#[must_use]
pub fn is_running(processes: &HashSet<ProcessKey>, path: &Path) -> bool {
    is_running_key(processes, path)
        || std::fs::canonicalize(path).is_ok_and(|canonical| is_running_key(processes, &canonical))
}

#[cfg(target_os = "windows")]
fn is_running_key(processes: &HashSet<ProcessKey>, path: &Path) -> bool {
    processes.contains(&normalize_windows_path(path))
}

#[cfg(not(target_os = "windows"))]
fn is_running_key(processes: &HashSet<ProcessKey>, path: &Path) -> bool {
    processes.contains(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_paths_are_normalised() {
        use super::normalize_windows_path;
        use std::path::Path;

        assert_eq!(
            normalize_windows_path(Path::new(r"\\?\UNC\Server\Share\App.exe")),
            r"\\server\share\app.exe"
        );
        assert_eq!(
            normalize_windows_path(Path::new(r"\\?\C:/Apps/App.exe")),
            r"c:\apps\app.exe"
        );
        assert_eq!(
            normalize_windows_path(Path::new(r"C:\Apps\App.exe")),
            r"c:\apps\app.exe"
        );
    }

    #[test]
    fn enumerating_processes_never_panics() {
        let _ = running_processes();
    }
}
