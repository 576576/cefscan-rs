//! 从 Windows 系统关联里取出可执行文件的图标，编码成 PNG data URL 交给前端。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// 结果按路径缓存。
static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

/// 返回 `data:image/png;base64,...`；取不到图标时返回 `None`。
pub fn data_url(path: &Path) -> Option<String> {
    let key = path.to_string_lossy().into_owned();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache.lock().unwrap();
        if let Some(cached) = guard.get(&key) {
            return cached.clone();
        }
    }
    let value = imp::extract(path);
    cache.lock().unwrap().insert(key, value.clone());
    value
}

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::c_void;
    use std::iter::once;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;
    use std::sync::Mutex;

    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC,
        GetDIBits, GetObjectW, HBITMAP, HDC, ReleaseDC,
    };
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows_sys::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

    /// 图标最大边长，超过就截断。
    const MAX_SIDE: i32 = 128;

    // `SHGetFileInfoW` 要求调用线程先初始化 COM，用 thread-local 记录是否已初始化。
    thread_local! {
        static COM_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn ensure_com() {
        COM_READY.with(|ready| {
            if ready.get() {
                return;
            }
            // SAFETY: 传 null 表示按系统默认方式初始化当前线程；返回值（含
            // RPC_E_CHANGED_MODE）在这里都无所谓，能拿到图标就行。
            unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
            ready.set(true);
        });
    }

    pub(super) fn extract(path: &Path) -> Option<String> {
        let (rgba, side) = capture(path)?;
        encode_png(&rgba, side)
    }

    /// 取图标的 RGBA 像素。**必须串行调用。**
    fn capture(path: &Path) -> Option<(Vec<u8>, u32)> {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let hicon = icon_handle(path)?;
        let pixels = render(hicon);
        // SAFETY: hicon 由 SHGetFileInfoW 产出，这里负责唯一一次销毁。
        unsafe { DestroyIcon(hicon) };
        pixels
    }

    /// 问 shell 要图标句柄。调用方负责 `DestroyIcon`。
    fn icon_handle(path: &Path) -> Option<HICON> {
        ensure_com();

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
        let mut info: SHFILEINFOW = unsafe { zeroed() };
        // SAFETY: wide 是 NUL 结尾的 UTF-16；info 是栈上缓冲区，且 cbfileinfo
        // 报的正是它的大小。传真实路径时 dwFileAttributes 会被忽略。
        let ok = unsafe {
            SHGetFileInfoW(
                wide.as_ptr(),
                0,
                &mut info,
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            )
        };
        if ok == 0 || info.hIcon.is_null() {
            return None;
        }
        Some(info.hIcon)
    }

    /// 把 HICON 读成 RGBA 像素，返回 `(像素, 边长)`。
    fn render(hicon: HICON) -> Option<(Vec<u8>, u32)> {
        let mut info: ICONINFO = unsafe { zeroed() };
        // SAFETY: hicon 有效；info 是栈上可写结构。
        if unsafe { GetIconInfo(hicon, &mut info) } == 0 {
            return None;
        }
        // GetIconInfo 会复制出两张 GDI 位图，必须自己删。
        let color = info.hbmColor;
        let mask = info.hbmMask;

        let side = measure(color, mask);
        let pixels = read_pixels(color, mask, side);

        // SAFETY: 两个句柄都由 GetIconInfo 产出，且此前未删除。
        unsafe {
            if !color.is_null() {
                DeleteObject(color);
            }
            if !mask.is_null() {
                DeleteObject(mask);
            }
        }

        pixels.map(|rgba| (rgba, side as u32))
    }

    /// 量一下图标边长。
    fn measure(color: HBITMAP, mask: HBITMAP) -> i32 {
        let probe = if color.is_null() { mask } else { color };
        let mut bitmap: BITMAP = unsafe { zeroed() };
        // SAFETY: probe 是有效位图句柄；bitmap 是栈上缓冲区，大小与传入值一致。
        let ok = unsafe {
            GetObjectW(
                probe,
                size_of::<BITMAP>() as i32,
                &mut bitmap as *mut BITMAP as *mut c_void,
            )
        };
        if ok == 0 || bitmap.bmWidth <= 0 {
            return 32;
        }
        bitmap.bmWidth.clamp(1, MAX_SIDE)
    }

    /// 读彩色位图的像素，必要时按掩码补出 alpha。
    fn read_pixels(color: HBITMAP, mask: HBITMAP, side: i32) -> Option<Vec<u8>> {
        // 没有彩色位图时直接放弃。
        if color.is_null() {
            return None;
        }

        // SAFETY: 传 null 表示取整个屏幕的 DC，仅用于给 GetDIBits 当"设备"参数。
        let screen = unsafe { GetDC(null_mut()) };
        if screen.is_null() {
            return None;
        }

        let color_bgra = read_bgra(screen, color, side);
        // 现代 32bpp 图标自带 alpha 通道；老式图标这一列会全是 0。
        let has_alpha = color_bgra
            .as_ref()
            .is_some_and(|bytes| bytes.as_chunks::<4>().0.iter().any(|px| px[3] != 0));
        let mask_bgra = if has_alpha {
            None
        } else {
            read_bgra(screen, mask, side)
        };

        // SAFETY: screen 由上面的 GetDC 产出，配套释放。
        unsafe { ReleaseDC(null_mut(), screen) };

        let color_bgra = color_bgra?;
        let mut rgba = Vec::with_capacity(color_bgra.len());
        for (index, px) in color_bgra.as_chunks::<4>().0.iter().enumerate() {
            let alpha = if has_alpha {
                px[3]
            } else {
                // 掩码里白色 = 透明，黑色 = 不透明；没有掩码就按全不透明算。
                match mask_bgra.as_ref().filter(|m| m.len() >= index * 4 + 4) {
                    Some(mask) => {
                        let m = &mask[index * 4..index * 4 + 4];
                        if m[0] == 0 && m[1] == 0 && m[2] == 0 {
                            0xff
                        } else {
                            0
                        }
                    }
                    None => 0xff,
                }
            };
            // BGRA → RGBA
            rgba.extend_from_slice(&[px[2], px[1], px[0], alpha]);
        }
        Some(rgba)
    }

    /// `GetDIBits` 取 32bpp 自顶向下像素，返回 BGRA。
    fn read_bgra(hdc: HDC, bitmap: HBITMAP, side: i32) -> Option<Vec<u8>> {
        if bitmap.is_null() {
            return None;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: side,
                // 负高度 = 自顶向下。
                biHeight: -side,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buffer = vec![0u8; (side * side * 4) as usize];
        // SAFETY: buffer 放得下 side*side 个 32bpp 像素，info 描述的正是这块内存。
        let lines = unsafe {
            GetDIBits(
                hdc,
                bitmap,
                0,
                side as u32,
                buffer.as_mut_ptr() as *mut c_void,
                &mut info,
                DIB_RGB_COLORS,
            )
        };
        if lines == 0 { None } else { Some(buffer) }
    }

    fn encode_png(rgba: &[u8], side: u32) -> Option<String> {
        use base64::Engine as _;

        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, side, side);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().ok()?;
            writer.write_image_data(rgba).ok()?;
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Some(format!("data:image/png;base64,{encoded}"))
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use std::path::Path;

    pub(super) fn extract(_path: &Path) -> Option<String> {
        None
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use std::path::PathBuf;

    use base64::Engine as _;

    use super::*;

    /// 拿测试进程自己的 exe 当样本。
    fn sample_exe() -> PathBuf {
        std::env::current_exe().expect("测试进程自己的路径")
    }

    fn decode(url: &str) -> Vec<u8> {
        let payload = url
            .strip_prefix("data:image/png;base64,")
            .expect("data URL 前缀");
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("base64 应当解得开")
    }

    #[test]
    fn a_real_executable_yields_a_png_data_url() {
        let url = data_url(&sample_exe()).expect("exe 应当有图标");
        let bytes = decode(&url);
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG 签名不对");

        // IHDR 紧跟在签名后面：8 字节签名 + 4 字节块长 + 4 字节块类型，之后才是宽高。
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        assert_eq!(width, height, "图标应当是正方形");
        assert!((16..=128).contains(&width), "边长 {width} 超出预期");
    }

    /// 断言图标不是全透明的。
    #[test]
    fn the_icon_actually_has_opaque_pixels() {
        let bytes = decode(&data_url(&sample_exe()).expect("exe 应当有图标"));
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .expect("PNG 头应当能解析");
        let mut buffer = vec![0u8; reader.output_buffer_size().expect("缓冲区大小")];
        let info = reader.next_frame(&mut buffer).expect("应当能解码一帧");
        assert_eq!(info.color_type, png::ColorType::Rgba);

        let rgba = &buffer[..info.buffer_size()];
        assert!(
            rgba.as_chunks::<4>().0.iter().any(|px| px[3] > 0),
            "图标不该是全透明的"
        );
    }

    #[test]
    fn a_path_that_does_not_exist_yields_none() {
        let missing = PathBuf::from(r"C:\cefscan-does-not-exist\ghost.exe");
        assert_eq!(data_url(&missing), None);
    }

    /// 断言并发取同一个 exe 的图标不会失败。
    #[test]
    fn concurrent_extraction_never_fails() {
        let exe = sample_exe();
        let failures = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| (0..20).filter(|_| imp::extract(&exe).is_none()).count()))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .sum::<usize>()
        });
        assert_eq!(failures, 0, "并发取同一个 exe 的图标不该失败");
    }
}
