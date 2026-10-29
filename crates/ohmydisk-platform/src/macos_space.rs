//! Public Darwin attributes, kept separate from cleanup's inexpensive stat path.
use ohmydisk_core::{AttributionProvider, SpaceObservation, now_ms};
use std::{
    ffi::{CStr, CString},
    io,
    mem::{MaybeUninit, size_of_val},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const VOLUME: u32 = libc::ATTR_VOL_INFO
    | libc::ATTR_VOL_SIZE
    | libc::ATTR_VOL_SPACEAVAIL
    | libc::ATTR_VOL_SPACEUSED;
// Prefixes are hints only: every successful file result still needs its returned
// bit. Bound the process cache, and remember unsupported roots so a portable
// fallback does not issue a second syscall for every file on an unsupported FS.
static SCOPES: OnceLock<Mutex<Vec<(PathBuf, bool)>>> = OnceLock::new();

fn remember_scope(path: &Path, supported: bool) {
    if let Ok(mut scopes) = SCOPES.get_or_init(Mutex::default).lock() {
        scopes.retain(|(root, _)| root != path);
        if scopes.len() >= 128 {
            scopes.remove(0);
        }
        scopes.push((path.to_path_buf(), supported));
    }
}

fn scope_support(path: &Path) -> Option<bool> {
    SCOPES
        .get_or_init(Mutex::default)
        .lock()
        .ok()?
        .iter()
        .filter(|(root, _)| path.starts_with(root))
        .max_by_key(|(root, _)| root.components().count())
        .map(|(_, supported)| *supported)
}

fn native_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Path contains NUL"))
}

fn query(path: &Path, volume: u32, fork: u32) -> io::Result<[u64; 16]> {
    let path = native_path(path)?;
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS,
        volattr: volume,
        dirattr: 0,
        fileattr: 0,
        forkattr: fork,
    };
    let mut buffer = [0_u64; 16];
    // SAFETY: the path is NUL terminated, the attrlist is fully initialized,
    // and the initialized writable buffer outlives the synchronous syscall.
    // The documented packed values are decoded with copies below, not casts.
    let result = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut attributes).cast(),
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer),
            libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(buffer)
    }
}

fn buffer_bytes(buffer: &[u64]) -> &[u8] {
    // SAFETY: initialized u64 storage consists entirely of initialized bytes;
    // this borrowed view cannot outlive its backing allocation.
    unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast(), size_of_val(buffer)) }
}

struct Values<'a> {
    bytes: &'a [u8],
    offset: usize,
    volume: u32,
    fork: u32,
}
impl<'a> Values<'a> {
    fn new(bytes: &'a [u8]) -> io::Result<Self> {
        let mut values = Self {
            bytes,
            offset: 0,
            volume: 0,
            fork: 0,
        };
        let length = values.u32()? as usize;
        if !(24..=bytes.len()).contains(&length) {
            return Err(invalid());
        }
        values.bytes = &bytes[..length];
        if values.u32()? != libc::ATTR_CMN_RETURNED_ATTRS {
            return Err(invalid());
        }
        values.volume = values.u32()?;
        if values.u32()? != 0 || values.u32()? != 0 {
            return Err(invalid());
        }
        values.fork = values.u32()?;
        Ok(values)
    }
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let end = self.offset.checked_add(N).ok_or_else(invalid)?;
        let result = self.bytes.get(self.offset..end).ok_or_else(invalid)?;
        self.offset = end;
        result.try_into().map_err(|_| invalid())
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_ne_bytes(self.take()?))
    }
    fn size(&mut self) -> io::Result<u64> {
        u64::try_from(i64::from_ne_bytes(self.take()?)).map_err(|_| invalid())
    }
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Malformed macOS space attributes",
    )
}

pub(super) fn observe(path: &Path, volume_id: String) -> io::Result<SpaceObservation> {
    let native = native_path(path)?;
    let mut filesystem = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: a valid C path and correctly sized writable output buffer are
    // supplied; assume_init is reached only after statfs reports success.
    if unsafe { libc::statfs(native.as_ptr(), filesystem.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful statfs call initialized the whole output structure.
    let filesystem = unsafe { filesystem.assume_init() };
    let filesystem_name = filesystem.f_fstypename.map(i8::cast_unsigned);
    let apfs =
        CStr::from_bytes_until_nul(&filesystem_name).is_ok_and(|name| name.to_bytes() == b"apfs");
    let attributes = query(path, VOLUME, 0).and_then(|buffer| {
        let mut values = Values::new(buffer_bytes(&buffer))?;
        let required = libc::ATTR_VOL_SIZE | libc::ATTR_VOL_SPACEAVAIL;
        if values.volume & required != required || values.volume & !VOLUME != 0 || values.fork != 0
        {
            return Err(invalid());
        }
        let total = values.size()?;
        let available = values.size()?;
        let used = if values.volume & libc::ATTR_VOL_SPACEUSED != 0 {
            Some(values.size()?)
        } else {
            None
        };
        Ok((total, available, used))
    });
    let (total_bytes, available_bytes, used_bytes, source) =
        if let Ok((total, available, used)) = attributes {
            (
                total,
                available,
                used,
                "darwin_getattrlist_volume_size_spaceavail",
            )
        } else {
            // statfs reports raw filesystem blocks on the same basis. Foundation's
            // important-usage capacity includes potentially purgeable content and
            // must never be substituted into this additive accounting model.
            let block = u64::from(filesystem.f_bsize);
            let total = filesystem.f_blocks.checked_mul(block).ok_or_else(invalid)?;
            let available = filesystem.f_bavail.checked_mul(block).ok_or_else(invalid)?;
            (total, available, None, "darwin_statfs_blocks_bavail")
        };
    if available_bytes > total_bytes {
        return Err(invalid());
    }
    remember_scope(path, apfs);
    Ok(SpaceObservation {
        volume_id,
        total_bytes,
        available_bytes,
        captured_at: now_ms(),
        source: source.into(),
        capacity_scope: "filesystem_visible".into(),
        provider: if apfs {
            AttributionProvider::MacPrivateSize
        } else {
            AttributionProvider::Unsupported
        },
        used_bytes,
        caller_available_bytes: Some(available_bytes),
    })
}

pub(super) fn private_bytes(path: &Path) -> io::Result<Option<u64>> {
    let parent = path.parent().unwrap_or(path);
    let supported = match scope_support(path) {
        Some(supported) => supported,
        None => observe(parent, String::new())?.provider == AttributionProvider::MacPrivateSize,
    };
    if !supported {
        return Ok(None);
    }
    let buffer = match query(path, 0, libc::ATTR_CMNEXT_PRIVATESIZE) {
        Ok(buffer) => buffer,
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS | libc::ENOTSUP | libc::EINVAL)
            ) =>
        {
            remember_scope(parent, false);
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let mut values = Values::new(buffer_bytes(&buffer))?;
    if values.volume != 0 || values.fork & !libc::ATTR_CMNEXT_PRIVATESIZE != 0 {
        return Err(invalid());
    }
    if values.fork & libc::ATTR_CMNEXT_PRIVATESIZE == 0 {
        remember_scope(parent, false);
        return Ok(None);
    }
    values.size().map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativePlatform;
    use ohmydisk_core::Platform;
    use std::{
        fs::{self, File},
        io::Write,
    };

    #[test]
    fn native_observation_and_sparse_private_size_are_explicit() {
        let fixture = tempfile::tempdir().unwrap();
        let platform = NativePlatform::new();
        let observation = platform.space_observation(fixture.path()).unwrap().unwrap();
        assert!(observation.total_bytes > 0);
        assert!(observation.available_bytes <= observation.total_bytes);
        assert_eq!(observation.capacity_scope, "filesystem_visible");
        assert_eq!(
            observation.volume_id,
            platform.metadata(fixture.path()).unwrap().volume_id
        );
        let path = fixture.path().join("sparse");
        let mut file = File::create(&path).unwrap();
        file.set_len(128 * 1024 * 1024).unwrap();
        file.write_all(b"private-data").unwrap();
        file.sync_all().unwrap();
        drop(file);
        assert_eq!(platform.metadata(&path).unwrap().private_bytes, None);
        let measured = platform.scan_metadata(&path).unwrap();
        if let Some(private) = measured.private_bytes {
            assert!(private < measured.logical_bytes);
        }
        let mut bulk_private = None;
        platform
            .read_directory(fixture.path(), &mut |event| {
                if let ohmydisk_core::DirectoryEvent::Entry(entry) = event
                    && entry.path == path
                {
                    bulk_private = entry
                        .info
                        .and_then(Result::ok)
                        .and_then(|info| info.private_bytes);
                }
                true
            })
            .unwrap();
        if let (Some(bulk), Some(fallback)) = (bulk_private, measured.private_bytes) {
            assert_eq!(
                bulk, fallback,
                "bulk and single-file attributes use the same private-size unit"
            );
        }
        let clone = fixture.path().join("clone");
        let source = native_path(&path).unwrap();
        let destination = native_path(&clone).unwrap();
        // SAFETY: both C paths are valid through clonefile, and zero is a valid flags value.
        if unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) } == 0 {
            let source_private = platform.scan_metadata(&path).unwrap().private_bytes;
            let clone_private = platform.scan_metadata(&clone).unwrap().private_bytes;
            if let (Some(source_private), Some(clone_private)) = (source_private, clone_private) {
                assert!(source_private < measured.logical_bytes);
                assert!(clone_private < measured.logical_bytes);
            }
        }
        fs::hard_link(&path, fixture.path().join("hardlink")).unwrap();
        assert_eq!(platform.scan_metadata(&path).unwrap().private_bytes, None);
    }

    #[test]
    fn malformed_or_unsupported_attribute_values_are_not_zero_measurements() {
        for length in 0..24 {
            assert!(Values::new(&vec![0; length]).is_err());
        }
        let mut header = Vec::new();
        for value in [24_u32, libc::ATTR_CMN_RETURNED_ATTRS, 0, 0, 0, 0] {
            header.extend(value.to_ne_bytes());
        }
        let values = Values::new(&header).unwrap();
        assert_eq!(values.fork & libc::ATTR_CMNEXT_PRIVATESIZE, 0);
        header[..4].copy_from_slice(&25_u32.to_ne_bytes());
        assert!(Values::new(&header).is_err());
        header[..4].copy_from_slice(&24_u32.to_ne_bytes());
        assert!(Values::new(&header).unwrap().size().is_err());
    }

    #[test]
    fn unsupported_scope_skips_private_syscalls_for_missing_files() {
        let fixture = tempfile::tempdir().unwrap();
        remember_scope(fixture.path(), false);
        assert_eq!(
            private_bytes(&fixture.path().join("not-created")).unwrap(),
            None
        );
        assert_eq!(
            private_bytes(&fixture.path().join("child/also-not-created")).unwrap(),
            None
        );
    }

    #[test]
    fn full_clone_sharing_is_not_attributed_as_two_allocated_files() {
        let fixture = tempfile::tempdir().unwrap();
        let source_path = fixture.path().join("source");
        let clone_path = fixture.path().join("clone");
        let mut source = File::create(&source_path).unwrap();
        source.write_all(&vec![b'x'; 65_536]).unwrap();
        source.sync_all().unwrap();
        drop(source);
        let platform = NativePlatform::new();
        let before = platform.scan_metadata(&source_path).unwrap();
        let source = native_path(&source_path).unwrap();
        let clone = native_path(&clone_path).unwrap();
        // SAFETY: owned C paths remain alive through the synchronous clonefile call.
        if unsafe { libc::clonefile(source.as_ptr(), clone.as_ptr(), 0) } != 0 {
            return;
        }
        let original = platform.scan_metadata(&source_path).unwrap();
        let cloned = platform.scan_metadata(&clone_path).unwrap();
        assert_eq!(original.logical_bytes, cloned.logical_bytes);
        if let (Some(before), Some(original), Some(cloned)) = (
            before.private_bytes,
            original.private_bytes,
            cloned.private_bytes,
        ) {
            // Metadata can remain private even when the data stream is fully
            // shared. Require a reduction, without promising an exact zero.
            if before > 0 {
                assert!(original < before);
                assert!(cloned < before);
            }
        }
    }
}
