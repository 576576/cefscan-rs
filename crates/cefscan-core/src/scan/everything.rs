//! Everything IPC 后端（仅 Windows）。
//!
//! 协议编解码不在这里 —— 那些是纯字节处理，拆到了平台中立的
//! [`super::everything_codec`]，好让它们在 Linux / macOS 与 Miri 下也测得到。
//! 这个文件只留 Win32 那一半：找窗口、注册消息窗口、发查询、收回复。

use std::io;
use std::mem::size_of;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowW, GWLP_USERDATA, GetWindowLongPtrW,
    HWND_MESSAGE, MSG, MSGFLT_ALLOW, PM_REMOVE, PeekMessageW, RegisterClassExW, SMTO_ABORTIFHUNG,
    SendMessageTimeoutW, SetWindowLongPtrW, WM_COPYDATA, WNDCLASSEXW,
};

use super::everything_codec::{encode_query, parse_reply, to_wide_z};
use crate::candidate::classify_candidate_name;
use crate::filter::Filter;
use crate::model::{Candidate, ScanOptions};

/// 索引服务的展示名。
pub const SERVICE_NAME: &str = "Everything";

/// Everything 各版本的隐藏窗口类名，按兼容性顺序探测。
const EVERYTHING_WINDOW_CLASSES: [&str; 2] = [
    "EVERYTHING_TASKBAR_NOTIFICATION",
    "EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)",
];
const REPLY_WINDOW_CLASS: &str = "CEFSCAN_EVERYTHING_IPC";
/// `EVERYTHING_IPC_COPYDATAQUERYW`
const COPYDATA_QUERY_W: usize = 2;
const REPLY_ID: u32 = 0x4345_4644;
/// 一次查询同时覆盖 pak / libcef / libnode / Electron Framework 四类候选。
const SEARCH: &str = r#"file: <_100_|libcef|libnode|"Chromium Embedded Framework">"#;

/// 索引服务是否在场。
///
/// 只看隐藏窗口在不在，**不发查询**。
pub fn is_service_available() -> bool {
    find_service_window().is_some()
}

/// 按兼容性顺序找 Everything 的隐藏窗口。
fn find_service_window() -> Option<HWND> {
    EVERYTHING_WINDOW_CLASSES.iter().find_map(|class| {
        let class = to_wide_z(class);
        // SAFETY: 类名 NUL 结尾（`to_wide_z`）；窗口不存在时返回 null，由 find_map 跳过。
        let handle = unsafe { FindWindowW(class.as_ptr(), ptr::null()) };
        (!handle.is_null()).then_some(handle)
    })
}

/// 查询 Everything。成功时连服务名一起返回，供结果里展示。
pub fn query_candidates(options: &ScanOptions) -> io::Result<(Vec<Candidate>, &'static str)> {
    let timeout = options.index_timeout.max(Duration::from_millis(100));
    let reply = Arc::new(Mutex::new(None::<Vec<u8>>));

    let reply_window = ReplyWindow::create(Arc::clone(&reply))?;
    let query = encode_query(reply_window.handle() as u32, REPLY_ID, SEARCH);

    // SAFETY: 查询窗口句柄来自 FindWindowW；COPYDATASTRUCT 与查询体在调用期间有效。
    let sent = unsafe {
        let mut copy_data = COPYDATASTRUCT {
            dwData: COPYDATA_QUERY_W,
            cbData: query.len() as u32,
            lpData: query.as_ptr() as *mut _,
        };
        SendMessageTimeoutW(
            reply_window.everything,
            WM_COPYDATA,
            0,
            &raw mut copy_data as LPARAM,
            SMTO_ABORTIFHUNG,
            timeout.as_millis() as u32,
            ptr::null_mut(),
        )
    };
    if sent == 0 {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Everything did not accept the query (is it running?)",
        ));
    }

    reply_window.pump_until_reply(timeout)?;

    let bytes = reply
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Everything did not reply"))?;

    let items = parse_reply(&bytes)?;
    // 索引是全盘的，排除规则需在结果侧再筛一遍。
    let filter = Filter::new(options);
    let mut candidates = Vec::with_capacity(items.len().min(1024));
    for item in items {
        let Some(kind) = classify_candidate_name(&item.file_name) else {
            continue;
        };
        let mut path = item.path;
        path.push(&item.file_name);
        // 先过规则再碰磁盘：被排除的路径连 `is_file()` 都不做。
        if !filter.allows_path(&path) {
            continue;
        }
        // 索引可能过期，只接受仍然存在的路径。
        if path.is_file() {
            candidates.push(Candidate { path, kind });
        }
    }
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((candidates, SERVICE_NAME))
}

// ---------- 回复窗口 ----------

struct ReplyWindow {
    handle: HWND,
    everything: HWND,
    reply: Arc<Mutex<Option<Vec<u8>>>>,
}

impl ReplyWindow {
    fn create(reply: Arc<Mutex<Option<Vec<u8>>>>) -> io::Result<Self> {
        let everything = find_service_window().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Everything is not running (or the Lite build without IPC is installed)",
            )
        })?;

        let class = to_wide_z(REPLY_WINDOW_CLASS);
        // SAFETY: WNDCLASSEXW 各字段一致，lpfnWndProc 指向下面定义的窗口过程。
        unsafe {
            let mut info: WNDCLASSEXW = std::mem::zeroed();
            info.cbSize = size_of::<WNDCLASSEXW>() as u32;
            info.lpfnWndProc = Some(window_proc);
            info.hInstance = GetModuleHandleW(ptr::null()) as HINSTANCE;
            info.lpszClassName = class.as_ptr();
            // 已存在同名类时也会失败，忽略即可。
            let _ = RegisterClassExW(&raw const info);
        }

        // SAFETY: 类名已注册；消息专用窗口用 HWND_MESSAGE 作父窗口。
        let handle = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                GetModuleHandleW(ptr::null()) as HINSTANCE,
                ptr::null_mut(),
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: 窗口刚创建，写入 GWLP_USERDATA 安全。
        unsafe {
            SetWindowLongPtrW(
                handle,
                GWLP_USERDATA,
                Arc::into_raw(Arc::clone(&reply)) as isize,
            );
        }

        // 放开消息过滤器以接收 WM_COPYDATA。
        let _ = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::ChangeWindowMessageFilterEx(
                handle,
                WM_COPYDATA,
                MSGFLT_ALLOW,
                ptr::null_mut(),
            )
        };

        Ok(Self {
            handle,
            everything,
            reply,
        })
    }

    fn handle(&self) -> HWND {
        self.handle
    }

    /// 泵消息直到收到回复或超时。
    fn pump_until_reply(&self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.reply.lock().is_ok_and(|guard| guard.is_some()) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for the Everything reply",
                ));
            }
            // SAFETY: MSG 由 PeekMessageW 填充。
            let mut message: MSG = unsafe { std::mem::zeroed() };
            let has_message =
                unsafe { PeekMessageW(&raw mut message, self.handle, 0, 0, PM_REMOVE) };
            if has_message != 0 {
                let _ = unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::DispatchMessageW(
                        &raw const message,
                    )
                };
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

impl Drop for ReplyWindow {
    fn drop(&mut self) {
        // SAFETY: 句柄由 create() 创建且只在这里释放。
        unsafe {
            let raw = GetWindowLongPtrW(self.handle, GWLP_USERDATA);
            if raw != 0 {
                drop(Arc::from_raw(raw as *const Mutex<Option<Vec<u8>>>));
            }
            SetWindowLongPtrW(self.handle, GWLP_USERDATA, 0);
            DestroyWindow(self.handle);
        }
    }
}

/// 回复窗口的窗口过程：收到 `WM_COPYDATA` 就把数据拷进 user data。
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_COPYDATA {
            let copy_data = lparam as *const COPYDATASTRUCT;
            if !copy_data.is_null() {
                let reply_id = (*copy_data).dwData as u32;
                if reply_id == REPLY_ID {
                    let bytes = std::slice::from_raw_parts(
                        (*copy_data).lpData as *const u8,
                        (*copy_data).cbData as usize,
                    )
                    .to_vec();
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA);
                    if raw != 0 {
                        let slot = Arc::from_raw(raw as *const Mutex<Option<Vec<u8>>>);
                        if let Ok(mut guard) = slot.lock() {
                            *guard = Some(bytes);
                        }
                        // 归还所有权。
                        let _ = Arc::into_raw(slot);
                    }
                }
            }
            return 1;
        }
        DefWindowProcW(window, message, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_class_names_are_nul_terminated() {
        for class in EVERYTHING_WINDOW_CLASSES {
            let encoded = to_wide_z(class);
            assert_eq!(encoded.last(), Some(&0), "{class} 必须以 NUL 结尾");
            assert_eq!(encoded.len(), class.encode_utf16().count() + 1);
        }
        assert_eq!(to_wide_z(REPLY_WINDOW_CLASS).last(), Some(&0));
    }
}
