//! 从 Windows 系统关联里取出可执行文件的图标，编码成 PNG data URL 交给前端。
//!
//! 链路：`SHGetFileInfoW`（拿 HICON）→ `GetIconInfo`（拆出彩色位图与掩码）
//! → `GetDIBits` 取 32bpp BGRA → 补 alpha → PNG → base64。
//!
//! 为什么不用 `DrawIconEx` 画进 DIB：那条路是否保留 32bpp 图标的 alpha 通道
//! 取决于具体 GDI 实现，而 `GetDIBits` 拿到的是位图原始像素，行为确定。代价是
//! 老式"只有 AND 掩码、没有 alpha 通道"的图标要自己按掩码补透明度。
//!
//! 取图标这一段是**全局串行**的：`SHGetFileInfoW` 并发调用会偶发失败，
//! 细节见 `imp::capture`。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// 结果按路径缓存。同一个 exe 在结果列表里可能重复出现，而且每次
/// `SHGetFileInfoW` 都要碰一次 shell，缓存能省下可观的开销。
static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

/// 返回 `data:image/png;base64,...`；取不到图标时返回 `None`。
///
/// 失败会被当成正常结果缓存下来——同一个路径没必要反复去 shell 里问。
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

    /// 图标最大边长。`SHGFI_LARGEICON` 一般给 32，个别皮肤给到 48，再大就截。
    const MAX_SIDE: i32 = 128;

    // `SHGetFileInfoW` 要求调用线程先初始化 COM。同一线程重复初始化只会返回
    // `S_FALSE`，所以用 thread-local 挡一下，避免每次都白跑一趟。
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
        // 取像素要串行，编码不用，所以锁在 capture 里而不是这里。
        let (rgba, side) = capture(path)?;
        encode_png(&rgba, side)
    }

    /// 取图标的 RGBA 像素。**必须串行调用。**
    ///
    /// `SHGetFileInfoW` 对并发调用不安全：4 个线程同时问同一个 exe，240 次里
    /// 有 3 次直接返回 0（拿不到 HICON）。失败点在 shell 调用本身——同一轮实测
    /// 里 `GetIconInfo` / `GetDIBits` 都是 0 次失败，所以不是我们销毁句柄的问题。
    /// 加这把锁之后同样的并发跑到 0 失败。
    ///
    /// 代价可以忽略：结果本来就按路径缓存，一次扫描最多几十个不同的 exe。
    fn capture(path: &Path) -> Option<(Vec<u8>, u32)> {
        // 锁中毒说明上一次调用 panic 了；图标是可有可无的装饰，接着用就行。
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

    /// 量一下图标边长。单色图标的掩码是 AND+XOR 两半叠起来的，但那种图标
    /// 会在 `read_pixels` 里因为拿不到彩色位图被拒掉，这里不必特殊处理。
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
        // 纯单色图标没有彩色位图，我们没有可用的颜色，直接放弃。
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
                // 负高度 = 自顶向下，省掉一次上下翻转。
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

    /// 拿测试进程自己的 exe 当样本：一定有图标，而且路径稳定可复现。
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

    /// 这条是防"alpha 补错了"的：如果彩色位图没有 alpha 又没走掩码分支，
    /// 或者 GetDIBits 参数写错，整张图会是全透明——前端看上去就是一个空白格。
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

    /// 回归测试：`SHGetFileInfoW` 不能并发调用。
    ///
    /// 不加锁时实测 4 线程 240 次里有 3 次拿不到 HICON（`extract` 返回 None），
    /// 表现为界面上偶发少一个图标、测试偶发红。锁加在 `imp::capture` 里。
    /// 这里直接打 `extract` 而不是 `data_url`，否则会被结果缓存挡住、
    /// 根本走不到 shell 调用。
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
