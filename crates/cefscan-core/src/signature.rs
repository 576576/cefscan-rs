//! 二进制签名扫描。

use std::fs;
use std::io::{self, Read, Seek};
use std::path::Path;

use crate::model::AppKind;

/// 单次读取的块大小。
pub const CHUNK_SIZE: usize = 1024 * 1024;
/// 块之间的重叠字节数。
pub const OVERLAP: usize = 64;

/// 扫描风味。Mini 分支用于从 `libnode` 线索里区分 `MiniElectron` / `MiniBlink`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Standard,
    Mini,
}

#[derive(Clone)]
struct Rule {
    kind: AppKind,
    needle: &'static str,
    finder: memchr::memmem::Finder<'static>,
}

/// 预构建 `memmem::Finder` 的扫描器。
///
/// 内含一个可复用的读取缓冲：全盘扫描时每个候选文件都会走一遍 [`scan_read`]，
/// 若每次重新分配，就是「文件数 × 1 MiB」的 alloc + memset。
#[derive(Clone)]
pub struct SignatureScanner {
    standard: Vec<Rule>,
    mini: Vec<Rule>,
    buffer: Vec<u8>,
}

impl Default for SignatureScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl SignatureScanner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            standard: rules(&[
                (AppKind::Electron, "third_party/electron_node"),
                (AppKind::Electron, "register_atom_browser_web_contents"),
                (AppKind::Nwjs, "url-nwjs"),
                (AppKind::CefSharp, "CefSharp.Internals"),
                (AppKind::Cef, "cef_string_utf8_to_utf16"),
            ]),
            mini: rules(&[
                (AppKind::MiniElectron, "napi_create_buffer"),
                (AppKind::MiniBlink, "miniblink"),
            ]),
            buffer: Vec::new(),
        }
    }

    /// 扫描任意 `Read`。缓冲区随扫描器复用，首次调用后才分配。
    ///
    /// # Errors
    ///
    /// 读取出错时原样返回；调用方（目录检查）会把它当作「这个文件没查到」跳过。
    pub fn scan_read<R: Read>(
        &mut self,
        reader: &mut R,
        flavor: Flavor,
    ) -> io::Result<Option<(AppKind, &'static str)>> {
        let rules = match flavor {
            Flavor::Standard => &self.standard,
            Flavor::Mini => &self.mini,
        };

        if self.buffer.len() < CHUNK_SIZE + OVERLAP {
            self.buffer.resize(CHUNK_SIZE + OVERLAP, 0);
        }
        let buffer: &mut [u8] = &mut self.buffer;

        let mut retained = 0_usize;
        let mut best: Option<(AppKind, &'static str)> = None;

        loop {
            let read = match reader.read(&mut buffer[retained..]) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            let available = retained + read;
            if let Some(hit) = strongest_in_chunk(&buffer[..available], rules) {
                best = match best {
                    Some(current) if current.0.rank() >= hit.0.rank() => Some(current),
                    _ => Some(hit),
                };
                // Electron 是最高优先级，没有更强的可能，直接收工。
                if best.is_some_and(|(kind, _)| kind == AppKind::Electron) {
                    break;
                }
            }

            retained = available.min(OVERLAP);
            buffer.copy_within(available - retained..available, 0);
        }

        Ok(best)
    }

    /// 扫描一个文件。不是 ELF / PE / Mach-O 就直接跳过。
    ///
    /// # Errors
    ///
    /// 打开或读取失败时返回错误；调用方会跳过该文件。
    pub fn scan_file(
        &mut self,
        path: &Path,
        flavor: Flavor,
    ) -> io::Result<Option<(AppKind, &'static str)>> {
        let mut file = fs::File::open(path)?;
        let mut magic = [0_u8; 4];
        let read = file.read(&mut magic)?;
        if !is_executable_magic(&magic[..read]) {
            return Ok(None);
        }
        file.rewind()?;
        self.scan_read(&mut file, flavor)
    }
}

fn rules(entries: &[(AppKind, &'static str)]) -> Vec<Rule> {
    entries
        .iter()
        .map(|(kind, needle)| Rule {
            kind: *kind,
            needle,
            finder: memchr::memmem::Finder::new(needle.as_bytes()).into_owned(),
        })
        .collect()
}

fn strongest_in_chunk(chunk: &[u8], rules: &[Rule]) -> Option<(AppKind, &'static str)> {
    let mut best: Option<(AppKind, &'static str)> = None;
    for rule in rules {
        if rule.finder.find(chunk).is_some() {
            let candidate = (rule.kind, rule.needle);
            best = match best {
                Some(current) if current.0.rank() >= candidate.0.rank() => Some(current),
                _ => Some(candidate),
            };
        }
    }
    best
}

/// ELF / PE / Mach-O 的 magic 检查。
#[must_use]
pub fn is_executable_magic(bytes: &[u8]) -> bool {
    if bytes.len() < 2 {
        return false;
    }
    if bytes.starts_with(b"\x7fELF") || bytes.starts_with(b"MZ") {
        return true;
    }
    if bytes.len() < 4 {
        return false;
    }
    matches!(
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        0xfeed_face
            | 0xcefa_edfe
            | 0xfeed_facf
            | 0xcffa_edfe
            | 0xcafe_babe
            | 0xbeba_feca
            | 0xcafe_babf
            | 0xbfba_feca
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn standard(bytes: &[u8]) -> Option<AppKind> {
        SignatureScanner::new()
            .scan_read(&mut Cursor::new(bytes.to_vec()), Flavor::Standard)
            .unwrap()
            .map(|(kind, _)| kind)
    }

    fn mini(bytes: &[u8]) -> Option<AppKind> {
        SignatureScanner::new()
            .scan_read(&mut Cursor::new(bytes.to_vec()), Flavor::Mini)
            .unwrap()
            .map(|(kind, _)| kind)
    }

    #[test]
    fn every_signature_maps_to_its_kind() {
        assert_eq!(
            standard(b"third_party/electron_node"),
            Some(AppKind::Electron)
        );
        assert_eq!(
            standard(b"register_atom_browser_web_contents"),
            Some(AppKind::Electron)
        );
        assert_eq!(standard(b"url-nwjs"), Some(AppKind::Nwjs));
        assert_eq!(standard(b"CefSharp.Internals"), Some(AppKind::CefSharp));
        assert_eq!(standard(b"cef_string_utf8_to_utf16"), Some(AppKind::Cef));
        assert_eq!(mini(b"napi_create_buffer"), Some(AppKind::MiniElectron));
        assert_eq!(mini(b"miniblink"), Some(AppKind::MiniBlink));
        assert_eq!(standard(b"nothing here"), None);
    }

    #[test]
    fn signatures_straddling_a_chunk_boundary_are_found() {
        let mut bytes = vec![0_u8; CHUNK_SIZE - 5];
        bytes.extend_from_slice(b"third_party/electron_node");
        assert_eq!(standard(&bytes), Some(AppKind::Electron));
    }

    #[test]
    fn the_strongest_signature_wins_across_chunks() {
        let mut bytes = b"cef_string_utf8_to_utf16".to_vec();
        bytes.resize(CHUNK_SIZE + 32, 0);
        bytes.extend_from_slice(b"url-nwjs");
        assert_eq!(standard(&bytes), Some(AppKind::Nwjs));
    }

    /// 缓冲复用后连续扫描仍然正确：大缓冲里残留的上一次内容不能被误判。
    #[test]
    fn the_scanner_can_be_reused_across_inputs() {
        let mut scanner = SignatureScanner::new();

        let mut big = vec![0_u8; CHUNK_SIZE + 128];
        big[CHUNK_SIZE..CHUNK_SIZE + 8].copy_from_slice(b"url-nwjs");
        assert_eq!(
            scanner
                .scan_read(&mut Cursor::new(big), Flavor::Standard)
                .unwrap()
                .map(|(kind, _)| kind),
            Some(AppKind::Nwjs)
        );

        assert_eq!(
            scanner
                .scan_read(&mut Cursor::new(b"nothing here".to_vec()), Flavor::Standard)
                .unwrap()
                .map(|(kind, _)| kind),
            None,
            "复用的缓冲不该把上一次的命中带过来"
        );
    }

    #[test]
    fn non_executable_bytes_are_rejected() {
        assert!(!is_executable_magic(b"#!"));
        assert!(!is_executable_magic(b""));
        assert!(!is_executable_magic(&[0x7f]));
        assert!(is_executable_magic(b"MZ\x90\x00"));
        assert!(is_executable_magic(b"\x7fELF"));
        for magic in [0xfeed_face_u32, 0xcffa_edfe, 0xcafe_babe, 0xbfba_feca] {
            assert!(is_executable_magic(&magic.to_be_bytes()));
        }
    }
}
