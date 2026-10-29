//! Win32 metadata, token queries, and native Shell reveal, behind safe wrappers.

use interprocess::{
    local_socket::ListenerOptions,
    os::windows::{local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor},
};
use ohmydisk_core::FileInfo;
use std::{
    fs::{File, OpenOptions},
    io,
    mem::{MaybeUninit, size_of},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};
use widestring::{U16CStr, U16CString};
use windows_sys::Win32::{
    Foundation::{HANDLE, LocalFree},
    Security::{
        Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE, FILE_ATTRIBUTE_TAG_INFO,
        FILE_BASIC_INFO, FILE_COMPRESSION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileAttributeTagInfo, FileBasicInfo,
        FileCompressionInfo, FileIdInfo, FileStandardInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx,
    },
    System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    },
};

pub fn current_user_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns a valid borrowed pseudo-handle.
    process_user_sid(unsafe { GetCurrentProcess() })
}

pub fn verify_process_user(process_id: u32) -> io::Result<()> {
    // SAFETY: query-only process open, no handle inheritance; invalid PID returns null.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful OpenProcess returned ownership of this valid handle.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    if process_user_sid(process.as_raw_handle())? != current_user_sid()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "IPC peer belongs to another user",
        ));
    }
    Ok(())
}

fn process_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token = ptr::null_mut();
    // SAFETY: valid pseudo-process handle and writable output pointer; query access only.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned a valid handle with ownership transferred to us.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut bytes = 0;
    // SAFETY: zero-length initial query writes only the required buffer length.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut bytes,
        );
    }
    if bytes < size_of::<TOKEN_USER>() as u32 {
        return Err(io::Error::last_os_error());
    }
    // usize provides sufficient alignment for TOKEN_USER and its embedded SID pointers.
    let mut storage = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    // SAFETY: token is alive; the aligned buffer has at least `bytes` writable bytes.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful query initializes TOKEN_USER in the aligned output buffer.
    let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = ptr::null_mut();
    // SAFETY: the SID refers into live storage and the output pointer is writable.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Win32 returned a NUL-terminated UTF-16 allocation.
    let result = unsafe { U16CStr::from_ptr_str(sid) }
        .to_string()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    // SAFETY: ConvertSidToStringSidW allocations must be released with LocalFree.
    unsafe {
        LocalFree(sid.cast());
    }
    result
}

pub fn secure_listener_options(options: ListenerOptions<'_>) -> io::Result<ListenerOptions<'_>> {
    let sid = current_user_sid()?;
    let sddl = U16CString::from_str(format!("D:P(A;;GA;;;{sid})"))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(options.security_descriptor(SecurityDescriptor::deserialize(&sddl)?))
}

pub fn reveal(path: &Path) -> io::Result<()> {
    use windows_sys::Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            StationsAndDesktops::{
                GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
            },
        },
        UI::{
            Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName},
            WindowsAndMessaging::WSF_VISIBLE,
        },
    };
    let mut flags = MaybeUninit::<USEROBJECTFLAGS>::uninit();
    let mut needed = 0;
    // SAFETY: the process owns the borrowed window-station handle; exact output type and size.
    if unsafe {
        GetUserObjectInformationW(
            GetProcessWindowStation(),
            UOI_FLAGS,
            flags.as_mut_ptr().cast(),
            size_of::<USEROBJECTFLAGS>() as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "No graphical Windows session is available",
        ));
    }
    // SAFETY: the matching query succeeded.
    if unsafe { flags.assume_init() }.dwFlags & WSF_VISIBLE as u32 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "No graphical Windows session is available",
        ));
    }
    // SAFETY: reserved pointer is null and COM initialization applies to this thread only.
    let initialized = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    if initialized < 0 && initialized != RPC_E_CHANGED_MODE {
        return Err(io::Error::other(format!(
            "Cannot initialize Windows Shell: {initialized:#x}"
        )));
    }
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut pidl = ptr::null_mut();
    // SAFETY: path is NUL-terminated; output pointer is valid and COM is initialized.
    let parsed = unsafe {
        SHParseDisplayName(
            wide.as_ptr(),
            ptr::null_mut(),
            &mut pidl,
            0,
            ptr::null_mut(),
        )
    };
    let result = if parsed < 0 {
        Err(io::Error::other(format!(
            "Cannot parse Windows Shell item: {parsed:#x}"
        )))
    } else {
        // SAFETY: successful parse returned a fully-qualified PIDL; zero children selects it.
        let opened = unsafe { SHOpenFolderAndSelectItems(pidl, 0, ptr::null(), 0) };
        // SAFETY: SHParseDisplayName allocated this PIDL with the COM allocator.
        unsafe {
            CoTaskMemFree(pidl.cast());
        }
        if opened < 0 {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("Windows Explorer is unavailable: {opened:#x}"),
            ))
        } else {
            Ok(())
        }
    };
    if initialized >= 0 {
        // SAFETY: balance only this function's successful CoInitializeEx, on the same thread.
        unsafe {
            CoUninitialize();
        }
    }
    result
}

pub fn metadata(path: &Path) -> io::Result<FileInfo> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let (identity, volume_id) = file_identity(&file)?;
    let mut standard = MaybeUninit::<FILE_STANDARD_INFO>::uninit();
    let mut attributes = MaybeUninit::<FILE_ATTRIBUTE_TAG_INFO>::uninit();
    let mut basic = MaybeUninit::<FILE_BASIC_INFO>::uninit();
    // SAFETY: each information class matches its buffer type and size; the file remains alive.
    unsafe {
        if GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            standard.as_mut_ptr().cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        if GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            attributes.as_mut_ptr().cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        if GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: both calls succeeded and initialized their corresponding output structures.
    let standard = unsafe { standard.assume_init() };
    // SAFETY: the matching query above succeeded.
    let attributes = unsafe { attributes.assume_init() };
    // SAFETY: the matching basic-information query succeeded.
    let basic = unsafe { basic.assume_init() };
    let is_link_or_reparse = attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    let kind = if is_link_or_reparse {
        ohmydisk_core::NodeKind::Symlink
    } else if attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        ohmydisk_core::NodeKind::Directory
    } else {
        ohmydisk_core::NodeKind::File
    };
    let mut allocated_bytes = u64::try_from(standard.AllocationSize).ok();
    if attributes.FileAttributes & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0 {
        let mut compression = MaybeUninit::<FILE_COMPRESSION_INFO>::uninit();
        // SAFETY: exact output type and size, with a live borrowed file handle.
        allocated_bytes = if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileCompressionInfo,
                compression.as_mut_ptr().cast(),
                size_of::<FILE_COMPRESSION_INFO>() as u32,
            )
        } != 0
        {
            // SAFETY: successful matching query initialized the output.
            u64::try_from(unsafe { compression.assume_init() }.CompressedFileSize).ok()
        } else {
            None
        };
    }
    Ok(FileInfo {
        private_bytes: None,
        kind,
        logical_bytes: u64::try_from(standard.EndOfFile)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        allocated_bytes,
        modified_ns: u64::try_from(basic.LastWriteTime)
            .ok()
            .and_then(|ticks| ticks.checked_sub(116_444_736_000_000_000))
            .and_then(|ticks| ticks.checked_mul(100)),
        identity,
        volume_id,
        link_count: u64::from(standard.NumberOfLinks),
        is_link_or_reparse,
    })
}

pub(super) fn file_identity(file: &File) -> io::Result<(String, String)> {
    let mut id = MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: exact FILE_ID_INFO output buffer and live file handle.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            id.as_mut_ptr().cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } != 0
    {
        // SAFETY: the matching query succeeded.
        let id = unsafe { id.assume_init() };
        let bytes = id
            .FileId
            .Identifier
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        return Ok((
            format!("win128:{}:{bytes}", id.VolumeSerialNumber),
            format!("win:{}", id.VolumeSerialNumber),
        ));
    }
    // Some filesystems expose only the legacy 64-bit identity. Keep that namespace explicit.
    let mut legacy = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: exact output structure and a live borrowed handle.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), legacy.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the matching query succeeded.
    let legacy = unsafe { legacy.assume_init() };
    let id = (u64::from(legacy.nFileIndexHigh) << 32) | u64::from(legacy.nFileIndexLow);
    Ok((
        format!("win64:{}:{id}", legacy.dwVolumeSerialNumber),
        format!("win:{}", legacy.dwVolumeSerialNumber),
    ))
}
