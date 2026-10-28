//! Recycle-only Windows Shell integration.
//!
//! IFileOperation::DeleteItem (including trash 5.2.9's Windows backend) can select
//! permanent deletion. This module calls only ITransferSource::RecycleItem,
//! whose documented contract moves an item to the supplied recycle location.
//! A missing handler, unavailable destination, or transfer failure is an error;
//! there is no DeleteItem, RemoveItem, or filesystem-delete fallback.

use std::{
    ffi::OsString,
    io,
    marker::PhantomData,
    os::windows::ffi::OsStrExt,
    path::{Component, Path, PathBuf, Prefix},
};
use windows::{
    Win32::{
        Foundation::{PROPERTYKEY, RPC_E_CHANGED_MODE},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::Shell::{
            BHID_Transfer, COPYENGINE_E_USER_CANCELLED, FOLDERID_RecycleBinFolder, ILFree,
            IShellItem, ITransferAdviseSink, ITransferAdviseSink_Impl, ITransferSource,
            SHCreateItemFromIDList, SHCreateItemFromParsingName, SHGetKnownFolderIDList,
        },
    },
    core::{Error, HRESULT, PCWSTR, PWSTR, Ref, Result as WinResult, implement},
};

pub fn recycle(path: &Path) -> io::Result<()> {
    let shell_path = shell_parsing_path(path)?;
    // Keep initialization alive until all COM interfaces declared below have been released.
    let _apartment = ComApartment::new()?;
    let wide = shell_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    if wide[..wide.len() - 1].contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Path contains NUL",
        ));
    }
    // SAFETY: COM is initialized on this thread, and wide is a live NUL-terminated path.
    let source: IShellItem =
        unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None) }.map_err(shell_error)?;
    // BHID_Transfer is defined only for folders. Bind the containing folder, not a file.
    // SAFETY: source is a live COM interface; this returns its containing folder.
    let parent = unsafe { source.GetParent() }.map_err(shell_error)?;
    // SAFETY: parent is a folder; the generic return type fixes the requested COM IID.
    let transfer: ITransferSource =
        unsafe { parent.BindToHandler(None, &BHID_Transfer) }.map_err(shell_error)?;
    // FOLDERID_RecycleBinFolder is a virtual folder. Obtain its PIDL rather than a path
    // or SHGetKnownFolderItem (whose documented support excludes virtual folders).
    // SAFETY: null token selects the current user and the GUID is a known shell folder.
    let pidl = unsafe { SHGetKnownFolderIDList(&FOLDERID_RecycleBinFolder, 0, None) }
        .map_err(shell_error)?;
    // SAFETY: a successful known-folder query returned a live absolute PIDL.
    let destination: WinResult<IShellItem> = unsafe { SHCreateItemFromIDList(pidl) };
    // SAFETY: SHGetKnownFolderIDList transfers this PIDL allocation to the caller.
    unsafe {
        ILFree(Some(pidl));
    }
    let destination = destination.map_err(shell_error)?;
    let sink: ITransferAdviseSink = NoUiTransferSink.into();
    // SAFETY: live COM objects in the same apartment. Failing to install the sink aborts
    // before the operation, so provider error/confirmation handling is never interactive.
    let cookie = unsafe { transfer.Advise(&sink) }.map_err(shell_error)?;
    // SAFETY: source belongs to the folder used to bind the handler; destination is the
    // current user's system Recycle Bin. Flags=0 does not request fallback deletion.
    let outcome = unsafe { transfer.RecycleItem(&source, &destination, 0) };
    // SAFETY: cookie came from the successful Advise above; balance before releasing sink.
    let unadvise = unsafe { transfer.Unadvise(cookie) };
    // The generated binding also rejects a successful HRESULT with no returned IShellItem.
    // Only a concrete recycled destination is accepted as success.
    let _recycled = outcome.map_err(shell_error)?;
    unadvise.map_err(shell_error)?;
    Ok(())
}

fn shell_error(error: Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "System recycle-only operation is unavailable or failed; permanent deletion was not requested: {error}"
        ),
    )
}

struct ComApartment {
    uninitialize: bool,
    _thread: PhantomData<*mut ()>,
}
impl ComApartment {
    fn new() -> io::Result<Self> {
        // SAFETY: reserved pointer is absent; initialization applies to this thread only.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result.is_err() && result != RPC_E_CHANGED_MODE {
            return Err(shell_error(Error::from_hresult(result)));
        }
        Ok(Self {
            uninitialize: result.is_ok(),
            _thread: PhantomData,
        })
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninitialize {
            // SAFETY: balances our successful initialization on the same thread.
            unsafe {
                CoUninitialize();
            }
        }
    }
}

#[implement(ITransferAdviseSink)]
struct NoUiTransferSink;

#[allow(non_snake_case)]
impl ITransferAdviseSink_Impl for NoUiTransferSink_Impl {
    fn UpdateProgress(&self, _: u64, _: u64, _: i32, _: i32, _: i32, _: i32) -> WinResult<()> {
        Ok(())
    }
    fn UpdateTransferState(&self, _: u32) -> WinResult<()> {
        Ok(())
    }
    fn ConfirmOverwrite(
        &self,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> WinResult<()> {
        Err(Error::from_hresult(COPYENGINE_E_USER_CANCELLED))
    }
    fn ConfirmEncryptionLoss(&self, _: Ref<IShellItem>) -> WinResult<()> {
        Err(Error::from_hresult(COPYENGINE_E_USER_CANCELLED))
    }
    fn FileFailure(
        &self,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        error: HRESULT,
        _: PWSTR,
        _: u32,
    ) -> WinResult<()> {
        Err(Error::from_hresult(error))
    }
    fn SubStreamFailure(&self, _: Ref<IShellItem>, _: &PCWSTR, error: HRESULT) -> WinResult<()> {
        Err(Error::from_hresult(error))
    }
    fn PropertyFailure(
        &self,
        _: Ref<IShellItem>,
        _: *const PROPERTYKEY,
        error: HRESULT,
    ) -> WinResult<()> {
        Err(Error::from_hresult(error))
    }
}

/// Preserve native UTF-16 while translating only the verbatim disk prefix that
/// the Windows Shell does not accept. UNC/device/relative paths fail closed.
fn shell_parsing_path(path: &Path) -> io::Result<PathBuf> {
    let mut components = path.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Only local drive paths can be recycled",
                ));
            }
        },
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Recycle requires an absolute local path",
            ));
        }
    };
    if components.next() != Some(Component::RootDir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Recycle requires an absolute local path",
        ));
    }
    let mut result = PathBuf::from(OsString::from(format!("{}:\\", char::from(drive))));
    for component in components {
        match component {
            Component::Normal(name) => result.push(name),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Recycle path must be normalized",
                ));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_paths_preserve_native_names_and_refuse_nonlocal_sources() {
        assert_eq!(
            shell_parsing_path(Path::new(r"\\?\C:\literal;$(name)\quote.txt")).unwrap(),
            PathBuf::from(r"C:\literal;$(name)\quote.txt")
        );
        for path in [
            r"\\server\share\file",
            r"\\?\UNC\server\share\file",
            r"relative\file",
            r"C:relative",
            r"\\.\pipe\file",
            r"C:\parent\..\file",
        ] {
            assert!(shell_parsing_path(Path::new(path)).is_err(), "{path}");
        }
    }

    #[test]
    fn transfer_sink_refuses_interactive_or_failure_recovery() {
        use windows::Win32::Foundation::E_ACCESSDENIED;
        let sink: ITransferAdviseSink = NoUiTransferSink.into();
        // SAFETY: these are calls to our own no-dereference policy callbacks, with
        // intentionally absent optional items and a live empty rename buffer.
        unsafe {
            assert_eq!(
                sink.ConfirmOverwrite(None, None, PCWSTR::null())
                    .unwrap_err()
                    .code(),
                COPYENGINE_E_USER_CANCELLED
            );
            assert_eq!(
                sink.ConfirmEncryptionLoss(None).unwrap_err().code(),
                COPYENGINE_E_USER_CANCELLED
            );
            assert_eq!(
                sink.FileFailure(None, PCWSTR::null(), E_ACCESSDENIED, &mut [])
                    .unwrap_err()
                    .code(),
                E_ACCESSDENIED
            );
            assert_eq!(
                sink.SubStreamFailure(None, PCWSTR::null(), E_ACCESSDENIED)
                    .unwrap_err()
                    .code(),
                E_ACCESSDENIED
            );
        }
    }
}
