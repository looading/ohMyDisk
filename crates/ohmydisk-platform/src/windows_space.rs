//! Same-basis volume counters and conservative local NTFS attribution policy.
use ohmydisk_core::{AttributionProvider, SpaceObservation, now_ms};
use std::{
    fs::OpenOptions,
    io,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    ptr,
};
use widestring::U16CString;
use windows_sys::Win32::{
    Foundation::ERROR_PROC_NOT_FOUND,
    Storage::FileSystem::{
        DISK_SPACE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationByHandleW, GetVolumePathNameW,
    },
    System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
    System::SystemServices::FILE_SUPPORTS_BLOCK_REFCOUNTING,
    System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE},
};

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid native Windows volume counters",
    )
}

fn provider(filesystem: &[u16], flags: u32, drive: u32) -> AttributionProvider {
    if [DRIVE_FIXED, DRIVE_REMOVABLE].contains(&drive)
        && String::from_utf16_lossy(filesystem).eq_ignore_ascii_case("NTFS")
        && flags & FILE_SUPPORTS_BLOCK_REFCOUNTING == 0
    {
        AttributionProvider::NtfsUnsharedAllocation
    } else {
        AttributionProvider::Unsupported
    }
}

type SpaceQuery = unsafe extern "system" fn(*const u16, *mut DISK_SPACE_INFORMATION) -> i32;

fn load_space_query() -> io::Result<Option<SpaceQuery>> {
    // Dynamic lookup keeps the explicit caller-view fallback usable on older
    // systems where this public Windows 10 1809 API is absent at load time.
    // SAFETY: the static module name is NUL terminated; GetModuleHandle borrows
    // the already loaded process module and does not transfer ownership.
    let kernel = unsafe { GetModuleHandleW(windows_sys::w!("kernel32.dll")) };
    if kernel.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a valid borrowed module and a static NUL-terminated export name.
    let function = unsafe { GetProcAddress(kernel, c"GetDiskSpaceInformationW".as_ptr().cast()) };
    let Some(function) = function else {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_PROC_NOT_FOUND.cast_signed()) {
            Ok(None)
        } else {
            Err(error)
        };
    };
    // SAFETY: this export has exactly the documented Win32 SpaceQuery ABI.
    // The module remains loaded for the entire process lifetime.
    let function: SpaceQuery = unsafe { std::mem::transmute(function) };
    Ok(Some(function))
}

fn native_counters(root: &[u16]) -> io::Result<Option<DISK_SPACE_INFORMATION>> {
    let Some(function) = load_space_query()? else {
        return Ok(None);
    };
    let mut counters = DISK_SPACE_INFORMATION::default();
    // SAFETY: the root is NUL terminated and counters is an initialized, exact
    // ABI output structure that remains writable until this call returns.
    let result = unsafe { function(root.as_ptr(), &raw mut counters) };
    if result < 0 {
        Err(io::Error::other(format!(
            "GetDiskSpaceInformationW failed: {result:#x}"
        )))
    } else {
        Ok(Some(counters))
    }
}

fn volume_root(path: &Path) -> io::Result<Vec<u16>> {
    let path = U16CString::from_os_str(path.as_os_str()).map_err(|_| invalid())?;
    let mut root = vec![0_u16; 32_768];
    let root_capacity = u32::try_from(root.len()).map_err(|_| invalid())?;
    // SAFETY: the input path is NUL terminated and root has the declared writable
    // capacity. The result is consumed only after the API reports success.
    if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root_capacity) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let root_length = root
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(invalid)?;
    root.truncate(root_length + 1);
    Ok(root)
}

fn measured_counters(counters: &DISK_SPACE_INFORMATION) -> io::Result<(u64, u64, u64, u64)> {
    let cluster = u64::from(counters.SectorsPerAllocationUnit)
        .checked_mul(u64::from(counters.BytesPerSector))
        .filter(|bytes| *bytes != 0)
        .ok_or_else(invalid)?;
    let bytes = |units: u64| units.checked_mul(cluster).ok_or_else(invalid);
    let total = bytes(counters.ActualTotalAllocationUnits)?;
    let available = bytes(counters.ActualAvailableAllocationUnits)?;
    let used = bytes(counters.UsedAllocationUnits)?;
    let caller_available = bytes(counters.CallerAvailableAllocationUnits)?;
    if available > total || used > total || caller_available > total {
        return Err(invalid());
    }
    Ok((total, available, used, caller_available))
}

pub(super) fn observe(path: &Path) -> io::Result<SpaceObservation> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let (_, volume_id) = super::windows::file_identity(&file)?;
    let root = volume_root(path)?;
    let mut filesystem = [0_u16; 256];
    let filesystem_capacity = u32::try_from(filesystem.len()).map_err(|_| invalid())?;
    let mut flags = 0;
    // SAFETY: all optional outputs are null except correctly sized initialized
    // filesystem/flags buffers, and file owns a live attributes-readable handle.
    let known_filesystem = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut flags,
            filesystem.as_mut_ptr(),
            filesystem_capacity,
        )
    } != 0;
    // SAFETY: root contains the API-returned NUL-terminated volume mount path.
    let drive = unsafe { GetDriveTypeW(root.as_ptr()) };
    let filesystem_length = filesystem
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(invalid)?;
    let attribution = if known_filesystem {
        provider(&filesystem[..filesystem_length], flags, drive)
    } else {
        AttributionProvider::Unsupported
    };
    // A missing export is an older-system compatibility case. A present API's
    // HRESULT failure must remain an error instead of masquerading as absence.
    if let Some(counters) = native_counters(&root)? {
        let (total_bytes, available_bytes, used, caller_available) = measured_counters(&counters)?;
        return Ok(SpaceObservation {
            volume_id,
            total_bytes,
            available_bytes,
            captured_at: now_ms(),
            source: "win32_disk_space_information_actual".into(),
            capacity_scope: "filesystem_volume".into(),
            provider: attribution,
            used_bytes: Some(used),
            caller_available_bytes: Some(caller_available),
        });
    }
    let mut total = 0;
    let mut caller_available = 0;
    // SAFETY: root is NUL terminated and the two requested u64 output
    // pointers are valid through this synchronous call. Both counters
    // have the same caller/quota basis; volume-free is not mixed in.
    if unsafe {
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            &raw mut caller_available,
            &raw mut total,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if caller_available > total {
        return Err(invalid());
    }
    Ok(SpaceObservation {
        volume_id,
        total_bytes: total,
        available_bytes: caller_available,
        captured_at: now_ms(),
        source: "win32_disk_free_space_ex_caller".into(),
        capacity_scope: "caller_view".into(),
        provider: attribution,
        used_bytes: None,
        caller_available_bytes: Some(caller_available),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativePlatform;
    use ohmydisk_core::Platform;
    use std::{
        fs::{self, File},
        io::Write,
        path::{Component, Prefix},
    };
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_SPARSE};

    #[test]
    fn supported_local_ntfs_uses_actual_volume_space_without_silent_fallback() {
        let github_windows = std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true");
        let export = load_space_query().expect("Kernel32 export lookup must not fail");
        if github_windows {
            // The repository's Windows CI runner is Windows Server 2022, newer
            // than this API's documented Windows Server 2019 minimum.
            assert!(
                export.is_some(),
                "Windows 2022 must export GetDiskSpaceInformationW"
            );
        }
        if export.is_none() {
            return;
        }

        let fixture = tempfile::tempdir().unwrap();
        // Network/UNC temporary directories are outside this local-volume test.
        let local_drive_path = |path: &Path| {
            matches!(path.components().next(),
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        };
        if !local_drive_path(fixture.path()) {
            return;
        }
        let path = fixture.path().canonicalize().unwrap();
        if !local_drive_path(&path) {
            return;
        }
        let root = volume_root(&path).unwrap();
        assert!(root.len() >= 2);
        assert_eq!(root.last(), Some(&0));
        assert_eq!(root.get(root.len() - 2), Some(&u16::from(b'\\')));
        // SAFETY: volume_root returned an owned NUL-terminated volume mount path.
        let drive = unsafe { GetDriveTypeW(root.as_ptr()) };
        if drive != DRIVE_FIXED {
            return;
        }
        let mut filesystem = [0_u16; 256];
        // SAFETY: root and the initialized filesystem-name output have matching
        // lifetimes and sizes. All unused optional output pointers are null.
        let known = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                filesystem.as_mut_ptr(),
                256,
            )
        } != 0;
        if !known {
            assert!(
                !github_windows,
                "Cannot inspect the CI fixed temporary volume: {}",
                io::Error::last_os_error()
            );
            return;
        }
        let length = filesystem.iter().position(|unit| *unit == 0).unwrap();
        if !String::from_utf16_lossy(&filesystem[..length]).eq_ignore_ascii_case("NTFS") {
            return;
        }

        let counters = native_counters(&root)
            .expect("Present GetDiskSpaceInformationW must succeed for this owned local NTFS fixture; HRESULT errors must remain visible")
            .expect("Previously discovered export disappeared");
        let (capacity, available, _, _) = measured_counters(&counters).unwrap();
        assert!(capacity > 0 && available <= capacity);
        let observation = NativePlatform::new()
            .space_observation(&path)
            .expect("Native volume observation failed")
            .expect("Windows provider omitted its observation");
        assert_eq!(observation.capacity_scope, "filesystem_volume");
        assert_eq!(observation.source, "win32_disk_space_information_actual");
        assert!(observation.used_bytes.is_some());
        assert!(observation.caller_available_bytes.is_some());
        assert!(
            observation.total_bytes > 0 && observation.available_bytes <= observation.total_bytes
        );
        assert_eq!(observation.total_bytes, capacity);
        // Free-space samples can change between API calls; do not require an
        // exact match for available bytes while other system processes write.
    }

    #[test]
    fn only_local_ntfs_without_shared_cluster_support_is_attributed() {
        let ntfs = "NTFS".encode_utf16().collect::<Vec<_>>();
        assert_eq!(
            provider(&ntfs, 0, DRIVE_FIXED),
            AttributionProvider::NtfsUnsharedAllocation
        );
        assert_eq!(
            provider(&ntfs, FILE_SUPPORTS_BLOCK_REFCOUNTING, DRIVE_FIXED),
            AttributionProvider::Unsupported
        );
        assert_eq!(provider(&ntfs, 0, 4), AttributionProvider::Unsupported);
        assert_eq!(
            provider(&"ReFS".encode_utf16().collect::<Vec<_>>(), 0, DRIVE_FIXED),
            AttributionProvider::Unsupported
        );
        assert_eq!(
            provider(&[], 0, DRIVE_FIXED),
            AttributionProvider::Unsupported
        );
    }

    #[test]
    fn actual_and_caller_counters_never_share_a_quota_basis() {
        let mut counters = DISK_SPACE_INFORMATION {
            ActualTotalAllocationUnits: 1000,
            ActualAvailableAllocationUnits: 400,
            CallerTotalAllocationUnits: 100,
            CallerAvailableAllocationUnits: 20,
            UsedAllocationUnits: 600,
            SectorsPerAllocationUnit: 8,
            BytesPerSector: 512,
            ..DISK_SPACE_INFORMATION::default()
        };
        assert_eq!(
            measured_counters(&counters).unwrap(),
            (4_096_000, 1_638_400, 2_457_600, 81_920)
        );
        counters.ActualTotalAllocationUnits = u64::MAX;
        assert!(measured_counters(&counters).is_err());
        counters.ActualTotalAllocationUnits = 10;
        assert!(measured_counters(&counters).is_err());
        counters.BytesPerSector = 0;
        assert!(measured_counters(&counters).is_err());
    }

    #[test]
    fn native_volume_and_sparse_file_need_only_user_file_access() {
        let fixture = tempfile::tempdir().unwrap();
        let platform = NativePlatform::new();
        let observation = platform.space_observation(fixture.path()).unwrap().unwrap();
        assert!(observation.total_bytes > 0);
        assert!(observation.available_bytes <= observation.total_bytes);
        let path = fixture.path().join("sparse");
        let mut file = File::create(&path).unwrap();
        let mut returned = 0;
        // SAFETY: FSCTL_SET_SPARSE accepts an empty input/output, returned is a
        // writable DWORD, the owned file handle is live, and no async state is used.
        let sparse = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_SPARSE,
                ptr::null(),
                0,
                ptr::null_mut(),
                0,
                &raw mut returned,
                ptr::null_mut(),
            )
        } != 0;
        if sparse {
            file.set_len(128 * 1024 * 1024).unwrap();
            file.write_all(b"small").unwrap();
            file.sync_all().unwrap();
            drop(file);
            let info = platform.metadata(&path).unwrap();
            assert!(
                info.allocated_bytes
                    .is_some_and(|bytes| bytes < info.logical_bytes)
            );
            assert_eq!(info.private_bytes, None);
            fs::hard_link(&path, fixture.path().join("hardlink")).unwrap();
            assert_eq!(platform.metadata(&path).unwrap().link_count, 2);
        }
    }
}
