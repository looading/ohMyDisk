//! macOS directory enumeration with regular-file metadata from one bulk call.
//!
//! The packed ABI is documented in Apple's getattrlist(2)/getattrlistbulk(2).
//! Attributes have four-byte alignment, including 64-bit values, so decoding
//! deliberately copies bytes instead of casting records to C/Rust structs.

use ohmydisk_core::{DirectoryEvent, FileInfo, NodeKind, ScanEntry};
use std::{
    ffi::OsStr,
    fs::OpenOptions,
    io,
    os::{fd::AsRawFd, unix::ffi::OsStrExt, unix::fs::OpenOptionsExt},
    path::Path,
};

// These two SDK constants are not exported by the libc crate.
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const VREG: u32 = 1;
const COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | ATTR_CMN_ERROR
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_FILEID;
const FILE: u32 =
    libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;
const REQUIRED_COMMON: u32 = COMMON & !ATTR_CMN_ERROR;
const INITIAL_BUFFER: usize = 16 * 1024;
const MAX_BUFFER: usize = 256 * 1024;
const MIN_RECORD: usize = 4 + 5 * 4;

pub(super) fn read_directory(
    path: &Path,
    visitor: &mut dyn FnMut(DirectoryEvent) -> bool,
) -> io::Result<()> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: COMMON,
        volattr: 0,
        dirattr: 0,
        fileattr: FILE,
        forkattr: 0,
    };
    enumerate(path, visitor, |buffer| {
        // SAFETY: directory owns a readable directory fd for this entire call.
        // attributes has the SDK layout and every field is initialized. buffer
        // is initialized, writable, eight-byte aligned, and has the supplied
        // byte length. Neither pointer is retained by getattrlistbulk.
        let result = unsafe {
            libc::getattrlistbulk(
                directory.as_raw_fd(),
                (&raw mut attributes).cast(),
                buffer.as_mut_ptr().cast(),
                std::mem::size_of_val(buffer),
                0,
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            usize::try_from(result).map_err(|_| invalid_record())
        }
    })
}

fn enumerate(
    path: &Path,
    visitor: &mut dyn FnMut(DirectoryEvent) -> bool,
    mut next_batch: impl FnMut(&mut [u64]) -> io::Result<usize>,
) -> io::Result<()> {
    // Small directories avoid clearing 256 KiB apiece. Wide directories grow
    // after full batches, and reuse the allocation for subsequent syscalls.
    let mut buffer = vec![0_u64; INITIAL_BUFFER / 8];
    let mut emitted = false;
    loop {
        let count = match next_batch(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.raw_os_error() == Some(libc::ERANGE) => {
                if buffer.len() * 8 == MAX_BUFFER {
                    return Err(error);
                }
                grow_buffer(&mut buffer);
                continue;
            }
            Err(error) if !emitted && unsupported(&error) => {
                // Restart only before publishing anything: replaying a partial
                // enumeration would double-count entries in the scanner.
                return ohmydisk_core::platform::read_directory_portable(path, visitor);
            }
            Err(error) => return Err(error),
        };
        if count == 0 {
            return Ok(());
        }
        let bytes = buffer_bytes(&buffer);
        if count > bytes.len() / MIN_RECORD {
            return Err(invalid_record());
        }
        let mut offset = 0_usize;
        for _ in 0..count {
            let mut header = Cursor::new(bytes.get(offset..).ok_or_else(invalid_record)?);
            let length = header.u32()? as usize;
            if length < MIN_RECORD || !length.is_multiple_of(8) {
                return Err(invalid_record());
            }
            let end = offset.checked_add(length).ok_or_else(invalid_record)?;
            let record = bytes.get(offset..end).ok_or_else(invalid_record)?;
            let parsed = parse_record(record)?;
            emitted = true;
            if !visitor(DirectoryEvent::Entry(ScanEntry {
                path: path.join(parsed.name),
                info: parsed.info.map(Ok),
            })) {
                return Ok(());
            }
            offset = end;
        }
        if offset >= bytes.len() * 3 / 4 && buffer.len() * 8 < MAX_BUFFER {
            grow_buffer(&mut buffer);
        }
    }
}

fn grow_buffer(buffer: &mut Vec<u64>) {
    buffer.resize((buffer.len() * 8 * 4).min(MAX_BUFFER) / 8, 0);
}

fn unsupported(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOSYS | libc::ENOTSUP | libc::EINVAL)
    )
}

fn buffer_bytes(buffer: &[u64]) -> &[u8] {
    // SAFETY: every byte of the initialized u64 slice is initialized and any
    // byte pattern is valid for u8. The byte view borrows the same allocation,
    // cannot outlive it, and is never alive while the next syscall mutates it.
    unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast(), std::mem::size_of_val(buffer)) }
}

struct ParsedEntry<'a> {
    name: &'a OsStr,
    info: Option<FileInfo>,
}

fn parse_record(record: &[u8]) -> io::Result<ParsedEntry<'_>> {
    let mut cursor = Cursor::new(record);
    if cursor.u32()? as usize != record.len() {
        return Err(invalid_record());
    }
    let common = cursor.u32()?;
    let volume = cursor.u32()?;
    let directory = cursor.u32()?;
    let file = cursor.u32()?;
    let fork = cursor.u32()?;
    let required = libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_NAME;
    if common & required != required
        || common & !COMMON != 0
        || volume != 0
        || directory != 0
        || file & !FILE != 0
        || fork != 0
    {
        return Err(invalid_record());
    }
    // ERROR, when present, precedes NAME even though its bitmap bit is higher.
    let error = if common & ATTR_CMN_ERROR != 0 {
        cursor.u32()?
    } else {
        0
    };
    let name_reference = cursor.position;
    let name_offset = cursor.i32()?;
    let name_length = cursor.u32()? as usize;
    let device = cursor.optional(common, libc::ATTR_CMN_DEVID, Cursor::i32)?;
    let object_type = cursor.optional(common, libc::ATTR_CMN_OBJTYPE, Cursor::u32)?;
    let modified = cursor.optional(common, libc::ATTR_CMN_MODTIME, |cursor| {
        Ok((cursor.i64()?, cursor.i64()?))
    })?;
    let inode = cursor.optional(common, libc::ATTR_CMN_FILEID, Cursor::u64)?;
    let links = cursor.optional(file, libc::ATTR_FILE_LINKCOUNT, Cursor::u32)?;
    let allocated = cursor.optional(file, libc::ATTR_FILE_ALLOCSIZE, Cursor::i64)?;
    let logical = cursor.optional(file, libc::ATTR_FILE_DATALENGTH, Cursor::i64)?;

    let start = name_reference
        .checked_add_signed(name_offset as isize)
        .ok_or_else(invalid_record)?;
    let end = start.checked_add(name_length).ok_or_else(invalid_record)?;
    if name_offset % 4 != 0 || start < cursor.position {
        return Err(invalid_record());
    }
    let name = record.get(start..end).ok_or_else(invalid_record)?;
    let Some((&0, name)) = name.split_last() else {
        return Err(invalid_record());
    };
    if name.is_empty() || name == b"." || name == b".." || name.contains(&0) || name.contains(&b'/')
    {
        return Err(invalid_record());
    }

    // Directories retain stat's nlink and mounted-root/firmlink semantics.
    // Symlinks, unusual types, per-entry errors and unsupported attributes are
    // all rechecked by the scanner's existing symlink_metadata path.
    let info = if error == 0
        && object_type == Some(VREG)
        && common & REQUIRED_COMMON == REQUIRED_COMMON
        && file & FILE == FILE
    {
        regular_info(device, inode, links, allocated, logical, modified)
    } else {
        None
    };
    Ok(ParsedEntry {
        name: OsStr::from_bytes(name),
        info,
    })
}

fn regular_info(
    device: Option<i32>,
    inode: Option<u64>,
    links: Option<u32>,
    allocated: Option<i64>,
    logical: Option<i64>,
    modified: Option<(i64, i64)>,
) -> Option<FileInfo> {
    // Match std's signed dev_t -> u64 cast, including its sign extension.
    #[allow(clippy::cast_sign_loss)]
    let device = device? as u64;
    let inode = inode?;
    let (seconds, nanos) = modified?;
    if !(0..1_000_000_000).contains(&nanos) {
        return None;
    }
    let modified_ns = u64::try_from(i128::from(seconds) * 1_000_000_000 + i128::from(nanos)).ok();
    let allocated = u64::try_from(allocated?).ok()?.checked_add(511)? & !511;
    Some(FileInfo {
        kind: NodeKind::File,
        logical_bytes: u64::try_from(logical?).ok()?,
        allocated_bytes: Some(allocated),
        modified_ns,
        identity: format!("unix:{device}:{inode}"),
        volume_id: format!("unix:{device}"),
        // XNU's stat ABI saturates its 16-bit st_nlink field.
        link_count: u64::from(links?.min(u32::from(u16::MAX))),
        is_link_or_reparse: false,
    })
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let end = self.position.checked_add(N).ok_or_else(invalid_record)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(invalid_record)?;
        let mut result = [0; N];
        result.copy_from_slice(bytes);
        self.position = end;
        Ok(result)
    }

    fn u32(&mut self) -> io::Result<u32> {
        self.take().map(u32::from_ne_bytes)
    }

    fn i32(&mut self) -> io::Result<i32> {
        self.take().map(i32::from_ne_bytes)
    }

    fn u64(&mut self) -> io::Result<u64> {
        self.take().map(u64::from_ne_bytes)
    }

    fn i64(&mut self) -> io::Result<i64> {
        self.take().map(i64::from_ne_bytes)
    }

    fn optional<T>(
        &mut self,
        bitmap: u32,
        flag: u32,
        read: impl FnOnce(&mut Self) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        if bitmap & flag != 0 {
            read(self).map(Some)
        } else {
            Ok(None)
        }
    }
}

fn invalid_record() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Malformed macOS bulk directory attributes",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativePlatform;
    use ohmydisk_core::Platform;
    use std::{
        collections::HashMap,
        fs::{self, File},
        io::{Seek, SeekFrom, Write},
        os::unix::{fs::symlink, net::UnixListener},
        process::Command,
        time::{Duration, UNIX_EPOCH},
    };

    #[test]
    fn real_files_match_metadata_and_nonfiles_use_fallback() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        fs::write(root.join("normal"), b"hello").unwrap();
        fs::write(root.join("empty"), []).unwrap();
        fs::write(root.join("é😀漢字"), b"unicode").unwrap();
        fs::hard_link(root.join("normal"), root.join("hardlink")).unwrap();
        fs::create_dir(root.join("directory")).unwrap();
        fs::write(root.join("directory/child"), []).unwrap();
        symlink("normal", root.join("symlink")).unwrap();
        symlink("directory", root.join("directory-link")).unwrap();
        symlink("missing", root.join("dangling")).unwrap();
        let _socket = UnixListener::bind(root.join("socket")).unwrap();
        let mut sparse = File::create(root.join("sparse")).unwrap();
        sparse.seek(SeekFrom::Start(128 * 1024 * 1024)).unwrap();
        sparse.write_all(b"x").unwrap();
        drop(sparse);
        fs::write(root.join("resource-fork"), b"data").unwrap();
        fs::write(
            root.join("resource-fork/..namedfork/rsrc"),
            vec![b'r'; 16_385],
        )
        .unwrap();
        File::create(root.join("pre-epoch"))
            .unwrap()
            .set_modified(UNIX_EPOCH - Duration::from_secs(100))
            .unwrap();
        fs::write(root.join("compress-input"), b"compressible".repeat(100_000)).unwrap();
        let compressed = Command::new("/usr/bin/ditto")
            .arg("--hfsCompression")
            .arg(root.join("compress-input"))
            .arg(root.join("compressed"))
            .output();
        if !compressed.is_ok_and(|output| output.status.success()) {
            // The test volume may not support transparent compression.
            let _ = fs::remove_file(root.join("compressed"));
        }
        let platform = NativePlatform::new();
        let mut entries = HashMap::new();
        platform
            .read_directory(root, &mut |event| {
                let DirectoryEvent::Entry(entry) = event else {
                    panic!("unexpected iteration error")
                };
                let expected = platform.metadata(&entry.path).unwrap();
                let actual = if expected.kind == NodeKind::File {
                    let info = entry.info.unwrap().unwrap();
                    assert_eq!(
                        serde_json::to_value(&info).unwrap(),
                        serde_json::to_value(&expected).unwrap(),
                        "{}",
                        entry.path.display()
                    );
                    info
                } else {
                    assert!(
                        entry.info.is_none(),
                        "nonfiles must retain metadata fallback"
                    );
                    expected
                };
                assert!(entries.insert(entry.path, actual).is_none());
                true
            })
            .unwrap();
        assert_eq!(entries.len(), fs::read_dir(root).unwrap().count());
        assert_eq!(entries[&root.join("resource-fork")].logical_bytes, 4);
        assert_eq!(
            entries[&root.join("sparse")].logical_bytes,
            128 * 1024 * 1024 + 1
        );
        assert_eq!(entries[&root.join("pre-epoch")].modified_ns, None);
        assert_eq!(
            entries[&root.join("normal")].identity,
            entries[&root.join("hardlink")].identity
        );
        assert_eq!(entries[&root.join("normal")].link_count, 2);
    }

    #[test]
    fn cancellation_stops_without_requesting_another_batch() {
        let temporary = tempfile::tempdir().unwrap();
        for index in 0..300 {
            fs::write(temporary.path().join(format!("item-{index}")), []).unwrap();
        }
        let mut count = 0;
        NativePlatform::new()
            .read_directory(temporary.path(), &mut |_| {
                count += 1;
                false
            })
            .unwrap();
        assert_eq!(count, 1);
        let bytes = record(b"item", COMMON, FILE, VREG, 0);
        let mut calls = 0;
        enumerate(temporary.path(), &mut |_| false, |buffer| {
            calls += 1;
            assert_eq!(calls, 1);
            fill_buffer(buffer, &bytes);
            Ok(1)
        })
        .unwrap();
        assert_eq!(calls, 1);
    }

    #[test]
    fn wide_directory_crosses_buffer_sizes_without_omitting_or_repeating_entries() {
        let temporary = tempfile::tempdir().unwrap();
        for index in 0..3000 {
            fs::write(temporary.path().join(format!("item-{index:04}")), []).unwrap();
        }
        let mut names = std::collections::HashSet::new();
        NativePlatform::new()
            .read_directory(temporary.path(), &mut |event| {
                let DirectoryEvent::Entry(entry) = event else {
                    panic!("unexpected iteration error");
                };
                assert!(entry.info.unwrap().is_ok());
                assert!(names.insert(entry.path));
                true
            })
            .unwrap();
        assert_eq!(names.len(), 3000);
    }

    #[test]
    fn refuses_a_symlink_directory() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("target")).unwrap();
        symlink("target", temporary.path().join("link")).unwrap();
        let mut visited = false;
        assert!(
            NativePlatform::new()
                .read_directory(&temporary.path().join("link"), &mut |_| {
                    visited = true;
                    true
                })
                .is_err()
        );
        assert!(!visited);
    }

    #[test]
    fn first_unsupported_call_falls_back_but_partial_results_are_not_replayed() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("item"), []).unwrap();
        let mut count = 0;
        enumerate(
            temporary.path(),
            &mut |event| {
                let DirectoryEvent::Entry(entry) = event else {
                    panic!("unexpected error")
                };
                assert!(entry.info.is_none());
                count += 1;
                true
            },
            |_| Err(io::Error::from_raw_os_error(libc::ENOTSUP)),
        )
        .unwrap();
        assert_eq!(count, 1);
        count = 0;
        let mut calls = 0;
        let bytes = record(b"item", COMMON, FILE, VREG, 0);
        let result = enumerate(
            temporary.path(),
            &mut |_| {
                count += 1;
                true
            },
            |buffer| {
                calls += 1;
                if calls == 1 {
                    fill_buffer(buffer, &bytes);
                    Ok(1)
                } else {
                    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
                }
            },
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ENOTSUP));
        assert_eq!(calls, 2);
        assert_eq!(count, 1);
    }

    #[test]
    fn interrupted_calls_retry_and_range_errors_grow_the_buffer() {
        let bytes = record(b"item", COMMON, FILE, VREG, 0);
        let mut calls = 0;
        let mut count = 0;
        enumerate(
            Path::new("/unused"),
            &mut |_| {
                count += 1;
                true
            },
            |buffer| {
                calls += 1;
                match calls {
                    1 => Err(io::Error::from_raw_os_error(libc::EINTR)),
                    2 => Err(io::Error::from_raw_os_error(libc::ERANGE)),
                    3 => {
                        assert_eq!(std::mem::size_of_val(buffer), 64 * 1024);
                        fill_buffer(buffer, &bytes);
                        Ok(1)
                    }
                    _ => Ok(0),
                }
            },
        )
        .unwrap();
        assert_eq!(calls, 4);
        assert_eq!(count, 1);
    }

    #[test]
    fn range_error_after_publication_grows_without_restarting() {
        let mut calls = 0;
        let mut names = Vec::new();
        enumerate(
            Path::new("/unused"),
            &mut |event| {
                let DirectoryEvent::Entry(entry) = event else {
                    panic!("unexpected iteration error");
                };
                names.push(entry.path);
                true
            },
            |buffer| {
                calls += 1;
                match calls {
                    1 => {
                        fill_buffer(buffer, &record(b"first", COMMON, FILE, VREG, 0));
                        Ok(1)
                    }
                    2 => Err(io::Error::from_raw_os_error(libc::ERANGE)),
                    3 => {
                        assert_eq!(std::mem::size_of_val(buffer), 64 * 1024);
                        fill_buffer(buffer, &record(b"second", COMMON, FILE, VREG, 0));
                        Ok(1)
                    }
                    _ => Ok(0),
                }
            },
        )
        .unwrap();
        assert_eq!(
            names,
            [Path::new("/unused/first"), Path::new("/unused/second")]
        );
        assert_eq!(calls, 4);
    }

    #[test]
    fn malformed_batches_and_nonunsupported_errors_fail_without_fallback() {
        let mut calls = 0;
        let error = enumerate(
            Path::new("/unused"),
            &mut |_| panic!("must not emit"),
            |_| {
                calls += 1;
                Err(io::Error::from_raw_os_error(libc::ERANGE))
            },
        )
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ERANGE));
        assert_eq!(calls, 3, "buffer growth must stop at 256 KiB");

        let error = enumerate(
            Path::new("/unused"),
            &mut |_| panic!("must not emit"),
            |_| Err(io::Error::from_raw_os_error(libc::EACCES)),
        )
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));

        let error = enumerate(
            Path::new("/unused"),
            &mut |_| panic!("must not emit"),
            |buffer| Ok(std::mem::size_of_val(buffer) / MIN_RECORD + 1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let error = enumerate(
            Path::new("/unused"),
            &mut |_| panic!("must not emit"),
            |buffer| {
                buffer[0] = u64::from(u32::MAX - 7);
                Ok(1)
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn missing_fields_entry_errors_and_nonregular_types_keep_the_entry() {
        for (common, file, object_type, error) in [
            (COMMON & !libc::ATTR_CMN_MODTIME, FILE, VREG, 0),
            (COMMON, FILE & !libc::ATTR_FILE_ALLOCSIZE, VREG, 0),
            (COMMON, FILE, VREG, u32::try_from(libc::EACCES).unwrap()),
            (COMMON, 0, 2, 0),
            (COMMON, FILE, 5, 0),
            (COMMON, FILE, 7, 0),
        ] {
            let bytes = record(b"item", common, file, object_type, error);
            let entry = parse_record(&bytes).unwrap();
            assert_eq!(entry.name, "item");
            assert!(entry.info.is_none());
        }
    }

    #[test]
    fn metadata_conversion_preserves_stat_edge_cases() {
        let info = regular_info(
            Some(-1),
            Some(42),
            Some(100_000),
            Some(513),
            Some(7),
            Some((-1, 0)),
        )
        .unwrap();
        assert_eq!(info.identity, format!("unix:{}:42", u64::MAX));
        assert_eq!(info.allocated_bytes, Some(1024));
        assert_eq!(info.link_count, 65_535);
        assert_eq!(info.modified_ns, None);
        assert!(
            regular_info(Some(1), Some(42), Some(1), Some(-1), Some(7), Some((1, 0))).is_none()
        );
        assert!(
            regular_info(Some(1), Some(42), Some(1), Some(1), Some(-1), Some((1, 0))).is_none()
        );
        assert!(
            regular_info(
                Some(1),
                Some(42),
                Some(1),
                Some(1),
                Some(7),
                Some((1, 1_000_000_000))
            )
            .is_none()
        );
        assert_eq!(
            regular_info(
                Some(1),
                Some(42),
                Some(1),
                Some(1),
                Some(7),
                Some((i64::MAX, 0))
            )
            .unwrap()
            .modified_ns,
            None
        );
    }

    #[test]
    fn malformed_records_are_rejected_without_dereferencing_offsets() {
        let valid = record(b"item", COMMON, FILE, VREG, 0);
        for length in 0..valid.len() {
            assert!(parse_record(&valid[..length]).is_err());
        }
        // A consistent leading length must still reject a truncated payload.
        let mut truncated = valid[..88].to_vec();
        truncated[..4].copy_from_slice(&88_u32.to_ne_bytes());
        assert!(parse_record(&truncated).is_err());
        // ERROR is packed first, so NAME's reference starts at byte 28.
        for offset in [i32::MIN, -32, 1, i32::MAX] {
            let mut bytes = valid.clone();
            bytes[28..32].copy_from_slice(&offset.to_ne_bytes());
            assert!(parse_record(&bytes).is_err());
        }
        let mut bytes = valid.clone();
        bytes[32..36].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(parse_record(&bytes).is_err());
        let mut bytes = valid.clone();
        bytes[4..8].copy_from_slice(&(COMMON | libc::ATTR_CMN_OWNERID).to_ne_bytes());
        assert!(parse_record(&bytes).is_err());
        for name in [b"".as_slice(), b".", b"..", b"a/b", b"a\0b"] {
            assert!(parse_record(&record(name, COMMON, FILE, VREG, 0)).is_err());
        }
        let mut bytes = valid;
        let offset =
            usize::try_from(i32::from_ne_bytes(bytes[28..32].try_into().unwrap())).unwrap();
        bytes[28 + offset + b"item".len()] = b'x';
        assert!(parse_record(&bytes).is_err());
    }

    #[test]
    fn filename_bytes_are_preserved_without_utf8_conversion() {
        let bytes = record(b"invalid-\xff", COMMON, FILE, VREG, 0);
        assert_eq!(
            parse_record(&bytes).unwrap().name.as_bytes(),
            b"invalid-\xff"
        );
    }

    fn fill_buffer(buffer: &mut [u64], record: &[u8]) {
        for (word, bytes) in buffer.iter_mut().zip(record.chunks_exact(8)) {
            *word = u64::from_ne_bytes(bytes.try_into().unwrap());
        }
    }

    fn record(name: &[u8], common: u32, file: u32, object_type: u32, error: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [0, common, 0, 0, file, 0] {
            bytes.extend(value.to_ne_bytes());
        }
        if common & ATTR_CMN_ERROR != 0 {
            bytes.extend(error.to_ne_bytes());
        }
        let reference = bytes.len();
        bytes.extend([0; 8]);
        if common & libc::ATTR_CMN_DEVID != 0 {
            bytes.extend(1_i32.to_ne_bytes());
        }
        if common & libc::ATTR_CMN_OBJTYPE != 0 {
            bytes.extend(object_type.to_ne_bytes());
        }
        if common & libc::ATTR_CMN_MODTIME != 0 {
            bytes.extend(1_i64.to_ne_bytes());
            bytes.extend(2_i64.to_ne_bytes());
        }
        if common & libc::ATTR_CMN_FILEID != 0 {
            bytes.extend(42_u64.to_ne_bytes());
        }
        if file & libc::ATTR_FILE_LINKCOUNT != 0 {
            bytes.extend(2_u32.to_ne_bytes());
        }
        if file & libc::ATTR_FILE_ALLOCSIZE != 0 {
            bytes.extend(4096_i64.to_ne_bytes());
        }
        if file & libc::ATTR_FILE_DATALENGTH != 0 {
            bytes.extend(7_i64.to_ne_bytes());
        }
        let offset = i32::try_from(bytes.len() - reference).unwrap();
        bytes[reference..reference + 4].copy_from_slice(&offset.to_ne_bytes());
        bytes[reference + 4..reference + 8]
            .copy_from_slice(&u32::try_from(name.len() + 1).unwrap().to_ne_bytes());
        bytes.extend(name);
        bytes.push(0);
        bytes.resize(bytes.len().next_multiple_of(8), 0);
        let length = u32::try_from(bytes.len()).unwrap();
        bytes[..4].copy_from_slice(&length.to_ne_bytes());
        bytes
    }
}
