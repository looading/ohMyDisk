//! Native filesystem integration. All destructive actions use the system trash API.

#[cfg(any(unix, test))]
use ohmydisk_core::NodeKind;
use ohmydisk_core::{DirectoryEvent, FileInfo, NativePath, Platform, SpaceObservation, Volume};
#[cfg(unix)]
use std::time::UNIX_EPOCH;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

#[cfg(target_os = "macos")]
mod macos_bulk;
#[cfg(target_os = "macos")]
mod macos_space;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_recycle;
#[cfg(windows)]
mod windows_space;
#[cfg(windows)]
pub use windows::verify_process_user;

#[derive(Clone, Copy, Debug, Default)]
pub struct NativePlatform;

impl NativePlatform {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Platform for NativePlatform {
    fn read_directory(
        &self,
        path: &Path,
        visitor: &mut dyn FnMut(DirectoryEvent) -> bool,
    ) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            macos_bulk::read_directory(path, visitor)
        }
        #[cfg(not(target_os = "macos"))]
        {
            ohmydisk_core::platform::read_directory_portable(path, visitor)
        }
    }

    fn metadata(&self, path: &Path) -> io::Result<FileInfo> {
        #[cfg(windows)]
        {
            windows::metadata(path)
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::symlink_metadata(path)?;
            let kind = node_kind(&metadata);
            Ok(FileInfo {
                private_bytes: None,
                kind,
                logical_bytes: metadata.len(),
                allocated_bytes: metadata.blocks().checked_mul(512),
                modified_ns: modified_ns(&metadata),
                identity: format!("unix:{}:{}", metadata.dev(), metadata.ino()),
                volume_id: format!("unix:{}", metadata.dev()),
                link_count: metadata.nlink(),
                is_link_or_reparse: metadata.file_type().is_symlink(),
            })
        }
    }

    fn scan_metadata(&self, path: &Path) -> io::Result<FileInfo> {
        let info = self.metadata(path)?;
        #[cfg(target_os = "macos")]
        let mut info = info;
        #[cfg(target_os = "macos")]
        if info.kind == NodeKind::File && !info.is_link_or_reparse && info.link_count == 1 {
            // Accounting is optional; a failed extra attribute query must not
            // discard successfully obtained metadata or an otherwise visible file.
            info.private_bytes = macos_space::private_bytes(path).ok().flatten();
        }
        Ok(info)
    }

    fn space_observation(&self, path: &Path) -> io::Result<Option<SpaceObservation>> {
        #[cfg(target_os = "macos")]
        {
            let info = self.metadata(path)?;
            macos_space::observe(path, info.volume_id).map(Some)
        }
        #[cfg(windows)]
        {
            windows_space::observe(path).map(Some)
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = path;
            Ok(None)
        }
    }

    fn volumes(&self) -> io::Result<Vec<Volume>> {
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let mut volumes = Vec::new();
        for disk in disks.list() {
            // IDs use the same device/volume identity as the scanner's metadata.
            let Ok(info) = self.metadata(disk.mount_point()) else {
                continue;
            };
            if volumes
                .iter()
                .any(|v: &Volume| v.mount_path == NativePath::new(disk.mount_point()))
            {
                continue;
            }
            let observation = self.space_observation(disk.mount_point()).ok().flatten();
            let (total_bytes, available_bytes) = observation
                .as_ref()
                .map_or((disk.total_space(), disk.available_space()), |space| {
                    (space.total_bytes, space.available_bytes)
                });
            volumes.push(Volume {
                space_observation: observation,
                id: info.volume_id,
                name: disk.name().to_string_lossy().into_owned(),
                mount_path: NativePath::new(disk.mount_point()),
                total_bytes,
                available_bytes,
            });
        }
        Ok(volumes)
    }

    fn trash(&self, path: &Path) -> io::Result<()> {
        // Never fall back to fs::remove_*: failure must preserve the user's file.
        #[cfg(windows)]
        return windows_recycle::recycle(path);
        #[cfg(target_os = "macos")]
        let result = {
            use trash::macos::{DeleteMethod, TrashContextExtMacos};
            let mut context = trash::TrashContext::new();
            // The Finder backend requires Apple Events authorization and may wait for a
            // prompt. NSFileManager supports a headless service without that extra approval.
            context.set_delete_method(DeleteMethod::NsFileManager);
            context.delete(path)
        };
        #[cfg(not(any(target_os = "macos", windows)))]
        let result = trash::delete(path);
        #[cfg(not(windows))]
        result.map_err(|error| io::Error::other(format!("System trash failed: {error}")))
    }

    fn reveal(&self, path: &Path) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Reveal requires an absolute path",
            ));
        }
        fs::symlink_metadata(path)?;
        #[cfg(target_os = "macos")]
        let status = Command::new("/usr/bin/open").arg("-R").arg(path).status()?;
        #[cfg(windows)]
        return windows::reveal(path);
        #[cfg(not(any(target_os = "macos", windows)))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "File manager reveal is supported on macOS and Windows",
        ));
        #[cfg(target_os = "macos")]
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("File manager is unavailable or no graphical session exists ({status})"),
            ))
        }
    }

    fn protected_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(home) = dirs::home_dir() {
            paths.push(home);
        }
        #[cfg(target_os = "macos")]
        paths.extend(
            [
                "/",
                "/System",
                "/Library",
                "/Applications",
                "/bin",
                "/sbin",
                "/usr",
                "/etc",
                "/private",
                "/dev",
                "/Volumes",
            ]
            .map(PathBuf::from),
        );
        #[cfg(windows)]
        {
            for variable in [
                "SystemRoot",
                "ProgramFiles",
                "ProgramFiles(x86)",
                "ProgramData",
            ] {
                if let Some(path) = std::env::var_os(variable) {
                    paths.push(path.into());
                }
            }
            if let Some(drive) = std::env::var_os("SystemDrive") {
                paths.push(PathBuf::from(drive).join("\\"));
            }
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        paths.extend(
            [
                "/", "/bin", "/sbin", "/usr", "/etc", "/dev", "/proc", "/sys", "/boot",
            ]
            .map(PathBuf::from),
        );
        // Both aliases and canonical paths matter on macOS (/etc -> /private/etc).
        let canonical = paths
            .iter()
            .filter_map(|path| fs::canonicalize(path).ok())
            .collect::<Vec<_>>();
        paths.extend(canonical);
        paths.sort();
        paths.dedup();
        paths
    }

    fn protected_subtrees(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        #[cfg(target_os = "macos")]
        paths.extend(
            [
                "/System",
                "/bin",
                "/sbin",
                "/usr",
                "/private/etc",
                "/private/var/db",
                "/private/var/root",
                "/private/var/run",
            ]
            .map(PathBuf::from),
        );
        #[cfg(windows)]
        for variable in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(path) = std::env::var_os(variable) {
                paths.push(path.into());
            }
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        paths.extend(
            [
                "/bin", "/sbin", "/usr", "/etc", "/dev", "/proc", "/sys", "/boot",
            ]
            .map(PathBuf::from),
        );
        let canonical = paths
            .iter()
            .filter_map(|path| fs::canonicalize(path).ok())
            .collect::<Vec<_>>();
        paths.extend(canonical);
        paths.sort();
        paths.dedup();
        paths
    }
}

#[cfg(unix)]
pub(crate) fn node_kind(metadata: &fs::Metadata) -> NodeKind {
    if metadata.file_type().is_symlink() {
        NodeKind::Symlink
    } else if metadata.is_dir() {
        NodeKind::Directory
    } else if metadata.is_file() {
        NodeKind::File
    } else {
        NodeKind::Other
    }
}

#[cfg(unix)]
pub(crate) fn modified_ns(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .try_into()
        .ok()
}

/// Create an application-owned private directory; reject symlinks and unsafe existing modes.
/// Existing directories are never silently chmod'ed.
pub fn prepare_private_dir(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "State directory must be absolute",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        match fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(path)?;
        // SAFETY: geteuid has no arguments, preconditions, or memory side effects.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "State directory must be a non-symlink directory owned by the current user with mode 0700",
            ));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        fs::create_dir_all(path)?;
        let metadata = fs::symlink_metadata(path)?;
        use std::os::windows::fs::MetadataExt;
        if !metadata.is_dir()
            || metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "State directory must not be a reparse point",
            ));
        }
        Ok(())
    }
}

/// OS user identity, used for endpoint names; never derived from a display username.
pub fn current_user_identity() -> io::Result<String> {
    #[cfg(windows)]
    {
        windows::current_user_sid()
    }
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        Ok(unsafe { libc::geteuid() }.to_string())
    }
}

/// Apply an explicit current-user DACL on Windows. Unix authenticates peer credentials.
pub fn secure_listener_options(
    options: interprocess::local_socket::ListenerOptions<'_>,
) -> io::Result<interprocess::local_socket::ListenerOptions<'_>> {
    #[cfg(windows)]
    {
        windows::secure_listener_options(options)
    }
    #[cfg(unix)]
    {
        Ok(options)
    }
}

/// Open a private coordination file without following a final symlink/reparse point.
pub fn open_private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    secure_file_options(&mut options);
    let file = options.open(path)?;
    validate_private_file(&file)?;
    Ok(file)
}

fn secure_file_options(options: &mut fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
}

fn validate_private_file(file: &fs::File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Coordination path must be a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Coordination file must be private to the current user",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Coordination file must not be a reparse point",
            ));
        }
    }
    Ok(())
}

/// Start the service independently of a GUI/stdio client's terminal or Windows Job Object.
/// Windows deliberately reports restricted Job Object breakaway instead of silently inheriting it.
pub fn spawn_service_process(data_dir: &Path) -> io::Result<Child> {
    let mut log_options = fs::OpenOptions::new();
    log_options.create(true).append(true);
    secure_file_options(&mut log_options);
    let log = log_options.open(data_dir.join("service.log"))?;
    validate_private_file(&log)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("service")
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the child hook calls only async-signal-safe setsid, with no allocation/locking.
        // It runs before exec in a newly forked child that is not yet a process-group leader.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(
            CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
        );
    }
    command.spawn().map_err(|error| io::Error::new(error.kind(), format!("Cannot start an independent background service; the host may prohibit Windows Job Object breakaway: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_native_file_and_hardlink_identity() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join(".hidden file;$(literal)");
        fs::write(&file, b"12345").unwrap();
        let first = NativePlatform.metadata(&file).unwrap();
        assert_eq!(first.logical_bytes, 5);
        assert_eq!(first.kind, NodeKind::File);
        assert!(!first.is_link_or_reparse);
        let other = fixture.path().join("hardlink");
        fs::hard_link(&file, &other).unwrap();
        assert_eq!(
            first.identity,
            NativePlatform.metadata(&other).unwrap().identity
        );
        assert_eq!(NativePlatform.metadata(&file).unwrap().link_count, 2);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_metadata_does_not_follow_target() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("target");
        let link = fixture.path().join("link");
        fs::write(&target, b"data").unwrap();
        symlink(&target, &link).unwrap();
        let info = NativePlatform.metadata(&link).unwrap();
        assert_eq!(info.kind, NodeKind::Symlink);
        assert!(info.is_link_or_reparse);
        assert_eq!(
            info.allocated_bytes,
            Some(fs::symlink_metadata(&link).unwrap().blocks() * 512)
        );
        assert_ne!(
            info.identity,
            NativePlatform.metadata(&target).unwrap().identity
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_directory_rejects_symlink_and_public_modes() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = tempfile::tempdir().unwrap();
        let private = fixture.path().join("private");
        prepare_private_dir(&private).unwrap();
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            prepare_private_dir(&private).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let alias = fixture.path().join("alias");
        symlink(&private, &alias).unwrap();
        assert!(prepare_private_dir(&alias).is_err());
    }

    #[test]
    fn protected_paths_include_home() {
        if let Some(home) = dirs::home_dir() {
            assert!(NativePlatform.protected_paths().contains(&home));
        }
    }
}
