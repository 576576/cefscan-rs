//! Everything IPC 的**纯字节编解码**。
//!
//! 这一层刻意不依赖任何 Windows API，这样编解码测试在 Linux / macOS 的 `cargo test`
//! 里也跑得到（真实调用点只有 Windows），并且可以被 Miri 解释执行 —— 见文件末尾的
//! `#[cfg(miri)]` 入口。Win32 那一半在 [`super::everything`]。
//!
//! 协议（Everything IPC）：
//!
//! - 查询体 = 5 个 `u32`（回复窗口句柄、回复 ID、两个保留位、`u32::MAX`）+ NUL 结尾的
//!   UTF-16 检索串。
//! - 回复体 = 7 个 `u32` 列表头 + 每条 3 个 `u32`（标志、文件名偏移、路径偏移），
//!   两个偏移都相对**数据区起点**（列表头 + 全部条目之后）。

use std::ffi::OsString;
use std::io;
use std::mem::size_of;
use std::path::PathBuf;

pub(super) const QUERY_HEADER_SIZE: usize = 5 * size_of::<u32>();
pub(super) const LIST_HEADER_SIZE: usize = 7 * size_of::<u32>();
pub(super) const ITEM_SIZE: usize = 3 * size_of::<u32>();
/// 回复里条目数的上限。协议本身不设限，一个畸形（或恶意）的回复可以声称有 40 亿条，
/// 直接 `with_capacity` 就是一次 OOM，所以先卡住再分配。
pub(super) const MAX_ITEM_COUNT: usize = 1_000_000;

/// 一条回复项。两处字符串都**必须**存在：偏移非法或没读到 NUL 都会让 [`parse_reply`] 报错，
/// 所以这里不用 `Option` 假装"可能缺失"。
pub(super) struct ReplyItem {
    pub(super) file_name: OsString,
    pub(super) path: PathBuf,
}

pub(super) fn encode_query(reply_window: u32, reply_id: u32, search: &str) -> Vec<u8> {
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

pub(super) fn parse_reply(bytes: &[u8]) -> io::Result<Vec<ReplyItem>> {
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
            file_name: wide_to_os_string(&read_utf16_z(bytes, file_name_offset, data_start)?),
            path: PathBuf::from(wide_to_os_string(&read_utf16_z(
                bytes,
                path_offset,
                data_start,
            )?)),
        });
    }
    Ok(items)
}

fn read_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let slice = bytes
        .get(offset..offset + size_of::<u32>())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "truncated Everything reply"))?;
    let array: [u8; 4] = slice
        .try_into()
        .expect("切片长度由上面的 get 保证是 4 字节");
    Ok(u32::from_le_bytes(array))
}

/// 读一个 NUL 结尾的 UTF-16 串，返回**不含**结尾 NUL 的码元。
///
/// 偏移非法或没读到 NUL 都是协议错误（`Err`），没有"字符串不存在"这种第三种结果，
/// 所以不套 `Option`。
fn read_utf16_z(bytes: &[u8], offset: u32, data_start: usize) -> io::Result<Vec<u16>> {
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
        let array: [u8; 2] = encoded
            .try_into()
            .expect("切片长度由上面的 get 保证是 2 字节");
        let unit = u16::from_le_bytes(array);
        if unit == 0 {
            return Ok(units);
        }
        units.push(unit);
        cursor += 2;
    }
}

pub(super) fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// 转成 UTF-16 并补上 NUL 结尾。
///
/// 和 [`to_wide`] 严格分开：Win32 的 `PCWSTR` 参数**必须** NUL 结尾，而
/// `OsString::from_wide` 不要。混用不会报错，只会静默匹配不上。
pub(super) fn to_wide_z(text: &str) -> Vec<u16> {
    let mut units = to_wide(text);
    units.push(0);
    units
}

/// UTF-16 码元转 `OsString`。
///
/// Windows 上就是原生表示（`from_wide`，零转换）；其它平台按有损 UTF-8 转 —— 那条分支
/// 只是为了让这个模块能在 Linux / macOS 的 `cargo test` 与 Miri 下跑到，真实调用点只有
/// Windows，所以永远不会走到。
fn wide_to_os_string(units: &[u16]) -> OsString {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStringExt as _;
        OsString::from_wide(units)
    }
    #[cfg(not(target_os = "windows"))]
    {
        String::from_utf16_lossy(units).into()
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
        assert_eq!(to_wide("ab"), vec![0x61, 0x62]);
        assert_eq!(to_wide_z("ab"), vec![0x61, 0x62, 0x00]);
        assert_eq!(to_wide_z(""), vec![0x00]);
    }

    #[test]
    fn reply_parser_reads_path_and_file_name() {
        let items =
            parse_reply(&reply_with_one_item(r"C:\Program Files\示例", "libcef.dll")).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].path,
            std::path::Path::new(r"C:\Program Files\示例")
        );
        assert_eq!(items[0].file_name, "libcef.dll");
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

    /// Miri 专用入口：用大量畸形输入把纯字节解析过一遍，确认没有越界 / UB。
    ///
    /// 普通 `cargo test` 不跑 —— Miri 慢两三个数量级，这个循环在原生下没有信息量。
    /// 跑法：`cargo +nightly miri test -p cefscan-core --lib -- miri_`
    #[cfg(miri)]
    #[test]
    fn miri_malformed_replies_never_panic() {
        // 挑有意义的长度：截断在头部之前 / 头部中间 / 条目区中间 / 正好一条。
        let lengths = [
            0,
            1,
            7,
            8,
            20,
            23,
            24,
            27,
            28,
            LIST_HEADER_SIZE,
            LIST_HEADER_SIZE + 1,
            LIST_HEADER_SIZE + ITEM_SIZE - 1,
            LIST_HEADER_SIZE + ITEM_SIZE,
            LIST_HEADER_SIZE + 2 * ITEM_SIZE,
        ];
        for len in lengths {
            for fill in [0x00_u8, 0x01, 0x7f, 0x80, 0xff] {
                let mut bytes = vec![fill; len];
                // 把条目数区域填成"声称有很多条"，逼 `data_start` 的 checked 路径。
                if len >= 24 {
                    bytes[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
                }
                let _ = parse_reply(&bytes);

                // 偏移指向每个可能的位置，确认 `read_utf16_z` 的边界检查都兜得住。
                for offset in 0..=len {
                    let mut probe = reply_with_one_item("a", "b");
                    let slot = LIST_HEADER_SIZE + 4;
                    probe[slot..slot + 4].copy_from_slice(&(offset as u32).to_le_bytes());
                    let _ = parse_reply(&probe);
                }
            }
        }
    }
}
