//! `cefscanw` 的 Tauri 2 外壳。
//!
//! 这里不含任何检测逻辑，只做三件事：接受前端的扫描请求、把 `cefscan-core`
//! 的结果流式推给前端、处理"在资源管理器中显示"。
//! GUI 与 CLI 是两个独立二进制，各自静态链接 core，互不依赖。

use std::path::PathBuf;

use cefscan_core::{AppInfo, Backend, ScanOptions, ScanStats};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

mod icon;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRequest {
    pub roots: Vec<String>,
    /// 搜索后端。GUI 不再提供选择，永远发 `auto`；字段留着是为了让这个命令
    /// 对脚本/其它调用方仍然是完整的（与 CLI 的 `--backend` 取值一致）。
    pub backend: Option<String>,
    pub threads: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRow {
    pub path: String,
    pub root: String,
    /// 从路径启发式推导出的可读应用名（`cefscan_core::display_name`）。
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub running: bool,
    pub evidence: Option<String>,
    /// `data:image/png;base64,...`，取不到图标时为 `null`。
    pub icon: Option<String>,
}

impl From<AppInfo> for AppRow {
    fn from(app: AppInfo) -> Self {
        Self {
            name: cefscan_core::display_name(&app.path),
            icon: icon::data_url(&app.path),
            path: app.path.to_string_lossy().into_owned(),
            root: app.root.to_string_lossy().into_owned(),
            kind: app.kind.label().to_owned(),
            size: app.size,
            running: app.running,
            evidence: app.evidence.map(str::to_owned),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ScanEvent {
    /// 后端刚选定，**先于任何结果**送达。
    ///
    /// 前端靠它把工具栏上的"自动"变成"自动（cefscan）"/"自动（Everything）"。
    /// 没有这个事件的话，用户只能等扫描结束才从汇总里看到后端是谁，
    /// 那"自动"这个选项就不可信了。
    Started {
        backend: String,
    },
    Item(AppRow),
    #[serde(rename_all = "camelCase")]
    Done {
        backend: String,
        apps: usize,
        total_bytes: u64,
        sum_bytes: u64,
        elapsed_ms: u64,
        dirs_scanned: u64,
    },
    Error {
        message: String,
    },
}

/// 流式扫描：每识别出一个应用就立刻推给前端，前端逐条渲染。
#[tauri::command]
async fn scan_apps(
    channel: Channel<ScanEvent>,
    request: Option<ScanRequest>,
) -> Result<(), String> {
    let request = request.unwrap_or(ScanRequest {
        roots: Vec::new(),
        backend: None,
        threads: None,
    });
    let options = to_options(&request);

    // Channel 不是 Copy，闭包要 move 进去，所以两个回调各克隆一份；
    // 外层保留原件发 Done / Error。
    let item_channel = channel.clone();
    let notice_channel = channel.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        // CPU/IO 密集，必须走 spawn_blocking，不能堵住 async 运行时。
        cefscan_core::scan_streaming(
            &options,
            |app| {
                let _ = item_channel.send(ScanEvent::Item(app.into()));
            },
            |notice| {
                let _ = notice_channel.send(ScanEvent::Started {
                    backend: notice.backend.to_owned(),
                });
            },
        )
    })
    .await
    .map_err(|error| format!("scan task failed: {error}"))?;

    match outcome {
        Ok(stats) => {
            let _ = channel.send(ScanEvent::Done {
                backend: stats.backend.to_owned(),
                apps: stats.apps,
                total_bytes: stats.total_bytes,
                sum_bytes: stats.sum_bytes,
                elapsed_ms: stats.elapsed_ms,
                dirs_scanned: stats.dirs_scanned,
            });
            Ok(())
        }
        Err(error) => {
            let message = error.to_string();
            let _ = channel.send(ScanEvent::Error {
                message: message.clone(),
            });
            Err(message)
        }
    }
}

/// 在资源管理器中选中某个路径。
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    reveal_in_explorer(&PathBuf::from(path))
}

fn to_options(request: &ScanRequest) -> ScanOptions {
    let backend = match request.backend.as_deref() {
        Some("index") => Backend::Index,
        // `filesystem` 是改名前的旧值，留作兼容。
        Some("cefscan" | "filesystem") => Backend::Filesystem,
        _ => Backend::Auto,
    };
    let threads = request.threads.unwrap_or(0);
    ScanOptions {
        roots: request.roots.iter().map(PathBuf::from).collect(),
        backend,
        walk_threads: threads,
        scan_threads: threads,
        ..ScanOptions::default()
    }
}

#[cfg(target_os = "windows")]
fn reveal_in_explorer(path: &std::path::Path) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    // 路径带空格时必须整体加引号，否则 explorer 会把参数拆开。
    let argument: Vec<u16> = OsStr::new("/select,\"")
        .encode_wide()
        .chain(path.as_os_str().encode_wide())
        .chain(OsStr::new("\"").encode_wide())
        .chain(once(0))
        .collect();
    let target: Vec<u16> = OsStr::new("explorer.exe")
        .encode_wide()
        .chain(once(0))
        .collect();
    let operation: Vec<u16> = OsStr::new("open").encode_wide().chain(once(0)).collect();

    // SAFETY: operation / target / argument 都是 NUL 结尾，且在本函数内保持有效。
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null::<HWND>() as HWND,
            operation.as_ptr(),
            target.as_ptr(),
            argument.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // 返回值 > 32 表示成功。
    if result as isize > 32 {
        Ok(())
    } else {
        Err(format!("无法在资源管理器中显示 {}", path.display()))
    }
}

#[cfg(not(target_os = "windows"))]
fn reveal_in_explorer(path: &std::path::Path) -> Result<(), String> {
    let directory = path.parent().unwrap_or(path);
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![scan_apps, reveal])
        .run(tauri::generate_context!())
        .expect("failed to launch cefscanw");
}

/// 让 `ScanStats` 的字段在 GUI 侧可用。
#[allow(dead_code)]
fn _assert_stats_used(stats: &ScanStats) -> u64 {
    stats.total_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ScanEvent` 的线上格式是**前端唯一依赖的契约**（`main.js` 里读
    /// `event.type` / `event.backend` / `event.totalBytes` …），但它跨的是
    /// Rust↔JS 这道没有类型检查的边界：改名或漏掉 `camelCase` 都不会有编译错误，
    /// 只会让界面静默失灵。所以这里把格式钉死。
    #[test]
    fn scan_events_keep_their_wire_format() {
        let json = |event: &ScanEvent| serde_json::to_value(event).unwrap();

        assert_eq!(
            json(&ScanEvent::Started {
                backend: "Everything".into()
            }),
            serde_json::json!({ "type": "started", "backend": "Everything" })
        );

        assert_eq!(
            json(&ScanEvent::Done {
                backend: "cefscan".into(),
                apps: 2,
                total_bytes: 10,
                sum_bytes: 12,
                elapsed_ms: 34,
                dirs_scanned: 56,
            }),
            serde_json::json!({
                "type": "done",
                "backend": "cefscan",
                "apps": 2,
                "totalBytes": 10,
                "sumBytes": 12,
                "elapsedMs": 34,
                "dirsScanned": 56,
            })
        );

        assert_eq!(
            json(&ScanEvent::Error {
                message: "boom".into()
            }),
            serde_json::json!({ "type": "error", "message": "boom" })
        );

        // `Item` 是 newtype variant：内部标签模式下会把 AppRow 摊平，
        // 所以前端拿到的是"AppRow 本身 + 一个 type 字段"。
        let item = json(&ScanEvent::Item(AppRow {
            path: r"C:\a\Demo.exe".into(),
            root: r"C:\a".into(),
            name: "Demo".into(),
            kind: "electron".into(),
            size: 1024,
            running: true,
            evidence: Some("Electron Framework".into()),
            icon: None,
        }));
        assert_eq!(item["type"], "item");
        assert_eq!(item["name"], "Demo");
        assert_eq!(item["size"], 1024);
        assert_eq!(item["evidence"], "Electron Framework");
        assert!(item["icon"].is_null());
    }
}
