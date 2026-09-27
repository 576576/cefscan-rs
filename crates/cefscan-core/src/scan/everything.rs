//! Everything IPC 后端（仅 Windows）。
//!
//! Everything 维护着一份全盘索引，查询是毫秒级的；代价是必须安装并运行
//! Everything（精简版没有 IPC，不支持）。拿不到结果时调用方会回落到文件系统遍历。
//!
//! 协议要点（<https://www.voidtools.com/support/everything/sdk/ipc/>）：
//! - 用 `WM_COPYDATA`（`dwData = 2`）把查询发给 Everything 的隐藏窗口
//! - 查询体是 5 个 `u32` 头 + UTF-16 NUL 结尾的搜索串
//! - Everything 用 `WM_COPYDATA` 把结果发回我们提供的回复窗口
//! - 回复体是 7 个 `u32` 头 + 每项 3 个 `u32`（flags / 文件名偏移 / 路径偏移）

use std::ffi::OsString;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowW, GWLP_USERDATA,
    GetWindowLongPtrW, HWND_MESSAGE, MSG, MSGFLT_ALLOW, PM_REMOVE, PeekMessageW,
    RegisterClassExW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, SetWindowLongPtrW, WM_COPYDATA,
    WNDCLASSEXW,
};

use crate::candidate::classify_candidate_name;
use crate::filter::Filter;
use crate::model::{Candidate, ScanOptions};

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

const QUERY_HEADER_SIZE: usize = 5 * size_of::<u32>();
const LIST_HEADER_SIZE: usize = 7 * size_of::<u32>();
const ITEM_SIZE: usize = 3 * size_of::<u32>();
const MAX_ITEM_COUNT: usize = 1_000_000;

/// 查询 Everything。
pub fn query_candidates(options: &ScanOptions) -> io::Result<Vec<Candidate>> {
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
            &mut copy_data as *mut _ as LPARAM,
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
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Everything did not reply"))?;

    let items = parse_reply(&bytes)?;
    // 索引是**全盘**的，而 `--root` 和排除规则是给遍历阶段准备的剪枝条件。
    // 走索引后端时没有遍历可剪，必须在结果侧再筛一遍，否则
    // `--backend index --root C:\Users\me` 会把整个磁盘的结果都吐出来。
    let filter = Filter::new(options);
    let mut candidates = Vec::with_capacity(items.len().min(1024));
    for item in items {
        let Some(name) = item.file_name else { continue };
        let Some(kind) = classify_candidate_name(&name) else {
            continue;
        };
        let path = match item.path {
            Some(directory) => {
                let mut full = directory;
                full.push(&name);
                full
            }
            None => PathBuf::from(OsString::from_wide(&to_wide(&name.to_string_lossy()))),
        };
        // 先过规则再碰磁盘：被排除的路径连 `is_file()` 都不做。
        if !filter.allows_path(&path) {
            continue;
        }
        // 索引可能过期；只接受仍然存在的路径。候选数量很少，这点开销可以忽略。
        if path.is_file() {
            candidates.push(Candidate { path, kind });
        }
    }
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(candidates)
}

// ---------- 协议编解码 ----------

fn encode_query(reply_window: u32, reply_id: u32, search: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(QUERY_HEADER_SIZE + search.len() * 2 + 2);
    for value in [reply_window, reply_id, 0, 0, u32::MAX] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for unit in to_wide(search) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes
}

struct ReplyItem {
    file_name: Option<OsString>,
    path: Option<PathBuf>,
}

fn parse_reply(bytes: &[u8]) -> io::Result<Vec<ReplyItem>> {
    if bytes.len() < LIST_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated Everything reply header",
        ));
    }
    let item_count = read_u32(bytes, 5 * size_of::<u32>())? as usize;
    if item_count > MAX_ITEM_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Everything reply contains too many items",
        ));
    }
    let data_start = item_count
        .checked_mul(ITEM_SIZE)
        .and_then(|size| LIST_HEADER_SIZE.checked_add(size))
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid item count in Everything reply",
            )
        })?;

    let mut items = Vec::with_capacity(item_count.min(4096));
    for index in 0..item_count {
        let offset = LIST_HEADER_SIZE + index * ITEM_SIZE;
        let file_name_offset = read_u32(bytes, offset + size_of::<u32>())?;
        let path_offset = read_u32(bytes, offset + 2 * size_of::<u32>())?;
        items.push(ReplyItem {
            file_name: read_utf16_z(bytes, file_name_offset, data_start)?
                .as_deref()
                .map(OsString::from_wide),
            path: read_utf16_z(bytes, path_offset, data_start)?
                .as_deref()
                .map(OsString::from_wide)
                .map(PathBuf::from),
        });
    }
    Ok(items)
}

fn read_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let slice = bytes.get(offset..offset + size_of::<u32>()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "truncated Everything reply")
    })?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn read_utf16_z(bytes: &[u8], offset: u32, data_start: usize) -> io::Result<Option<Vec<u16>>> {
    let mut cursor = offset as usize;
    if cursor < data_start || !cursor.is_multiple_of(2) || cursor >= bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid string offset in Everything reply",
        ));
    }
    let mut units = Vec::new();
    loop {
        let encoded = bytes.get(cursor..cursor + 2).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "unterminated UTF-16 string in Everything reply",
            )
        })?;
        let unit = u16::from_le_bytes(encoded.try_into().unwrap());
        if unit == 0 {
            return Ok(Some(units));
        }
        units.push(unit);
        cursor += 2;
    }
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// 转成 UTF-16 并补上 NUL 结尾。
///
/// Win32 里凡是收 `PCWSTR` 的参数（`FindWindowW` 的类名、`WNDCLASSEXW.lpszClassName`）
/// 都要求 NUL 结尾；漏了不会报错，只会静默匹配不上——所以单独一个函数，
/// 避免和"要拿 `Vec<u16>` 去构造 `OsString`"的场景混用。
fn to_wide_z(text: &str) -> Vec<u16> {
    let mut units = to_wide(text);
    units.push(0);
    units
}

// ---------- 回复窗口 ----------

struct ReplyWindow {
    handle: HWND,
    everything: HWND,
    reply: Arc<Mutex<Option<Vec<u8>>>>,
}

impl ReplyWindow {
    fn create(reply: Arc<Mutex<Option<Vec<u8>>>>) -> io::Result<Self> {
        // SAFETY: 类名与标题都是 NUL 结尾；找不到窗口时返回 null，由调用方处理。
        let everything = EVERYTHING_WINDOW_CLASSES
            .iter()
            .find_map(|class| {
                let class = to_wide_z(class);
                let handle = unsafe { FindWindowW(class.as_ptr(), ptr::null()) };
                (!handle.is_null()).then_some(handle)
            })
            .ok_or_else(|| {
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
            if RegisterClassExW(&info) == 0 {
                // 已存在同名类时也会失败，忽略即可。
            }
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
            SetWindowLongPtrW(handle, GWLP_USERDATA, Arc::into_raw(Arc::clone(&reply)) as isize);
        }

        // Everything 可能是更高完整性级别启动的；放开过滤器才收得到 WM_COPYDATA。
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
            if self.reply.lock().map(|guard| guard.is_some()).unwrap_or(false) {
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
                unsafe { PeekMessageW(&mut message, self.handle, 0, 0, PM_REMOVE) };
            if has_message != 0 {
                let _ = unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::DispatchMessageW(&message)
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
    _wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // 2024 edition 要求 unsafe fn 内部也显式写出 unsafe 块。
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
                        // 归还所有权：窗口存活期间 user data 里的 Arc 必须一直有效。
                        let _ = Arc::into_raw(slot);
                    }
                }
            }
            return 1;
        }
        DefWindowProcW(window, message, _wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_utf16_z(bytes: &mut Vec<u8>, text: &str) -> u32 {
        let offset = bytes.len() as u32;
        for unit in text.encode_utf16().chain(std::iter::once(0)) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        offset
    }

    fn reply_with_one_item(path: &str, file_name: &str) -> Vec<u8> {
        let mut bytes = vec![0_u8; LIST_HEADER_SIZE + ITEM_SIZE];
        let path_offset = push_utf16_z(&mut bytes, path);
        let file_name_offset = push_utf16_z(&mut bytes, file_name);
        for (index, value) in [0_u32, 1, 1, 0, 1, 1, 0].into_iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[LIST_HEADER_SIZE + 4..LIST_HEADER_SIZE + 8]
            .copy_from_slice(&file_name_offset.to_le_bytes());
        bytes[LIST_HEADER_SIZE + 8..LIST_HEADER_SIZE + 12]
            .copy_from_slice(&path_offset.to_le_bytes());
        bytes
    }

    #[test]
    fn query_has_packed_header_and_null_terminated_utf16() {
        let query = encode_query(0x1234, 0x4321, "libcef");
        assert_eq!(&query[0..4], &0x1234_u32.to_le_bytes());
        assert_eq!(&query[4..8], &0x4321_u32.to_le_bytes());
        assert_eq!(&query[12..20], &[0, 0, 0, 0, 255, 255, 255, 255]);
        assert_eq!(&query[query.len() - 2..], &[0, 0]);
    }

    #[test]
    fn only_the_c_string_helper_appends_a_nul() {
        // Win32 的 PCWSTR 参数要求 NUL 结尾，而 `OsString::from_wide` 不要。
        // 两者混用只会静默失败（FindWindowW 找不到窗口），所以钉死这个区别。
        assert_eq!(to_wide("ab"), vec![0x61, 0x62]);
        assert_eq!(to_wide_z("ab"), vec![0x61, 0x62, 0x00]);
        assert_eq!(to_wide_z(""), vec![0x00]);
    }

    #[test]
    fn window_class_names_are_nul_terminated() {
        for class in EVERYTHING_WINDOW_CLASSES {
            let encoded = to_wide_z(class);
            assert_eq!(encoded.last(), Some(&0), "{class} 必须以 NUL 结尾");
            assert_eq!(encoded.len(), class.encode_utf16().count() + 1);
        }
        assert_eq!(to_wide_z(REPLY_WINDOW_CLASS).last(), Some(&0));
    }

    #[test]
    fn reply_parser_reads_path_and_file_name() {
        let items = parse_reply(&reply_with_one_item(r"C:\Program Files\示例", "libcef.dll")).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path.as_deref(), Some(std::path::Path::new(r"C:\Program Files\示例")));
        assert_eq!(items[0].file_name.as_ref().unwrap(), "libcef.dll");
    }

    #[test]
    fn reply_parser_rejects_out_of_bounds_offsets() {
        let mut reply = reply_with_one_item(r"C:\App", "libcef.dll");
        reply[LIST_HEADER_SIZE + 4..LIST_HEADER_SIZE + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_reply(&reply).is_err());
    }

    #[test]
    fn reply_parser_limits_item_allocation() {
        let mut reply = vec![0_u8; LIST_HEADER_SIZE];
        reply[20..24].copy_from_slice(&1_000_001_u32.to_le_bytes());
        assert!(parse_reply(&reply).is_err());
    }
}
