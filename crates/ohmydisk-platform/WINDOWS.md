# Windows platform validation

The Windows Rust code has been cross-checked from macOS with Rust 1.94.1 and
`x86_64-pc-windows-msvc`. This checks the app, GUI, MCP, service, metadata, IPC,
and recycle-only COM integration. It is not evidence of Windows linking,
installation, execution, or availability on every filesystem.

## Recycle-only operation

Windows uses `ITransferSource::RecycleItem`, not the `trash` crate's Windows
`IFileOperation::DeleteItem` backend. The generic backend requests
`FOF_ALLOWUNDO | FOF_WANTNUKEWARNING`, whose documented semantics preserve undo
information if possible and warn before permanent deletion. Those flags do not
establish the application's required failure policy.

The implementation resolves a local absolute Shell item, gets its parent folder,
and binds that folder to `ITransferSource` using `BHID_Transfer`. The binding is
on the parent because the documented handler is available for folders. The
destination comes from the current user's `FOLDERID_RecycleBinFolder` PIDL,
converted to an `IShellItem`; no `$Recycle.Bin` paths or metadata are constructed.
The system's handler chooses the volume-specific recycling behavior.

The only modifying API called is `RecycleItem`. Its documented contract recycles
the source into the supplied recycling location and returns the new Shell item.
The implementation never calls `RemoveItem`, `DeleteItem`, or a filesystem
delete API as a fallback. A missing handler, unavailable virtual recycle
destination, denied access, unsupported volume, or transfer error returns an
error. A no-UI `ITransferAdviseSink` refuses overwrite/encryption confirmations
and returns failures to the caller. Success requires a returned recycled item.

This code has **not run on Windows**. A real Windows test must verify a normal
local file and directory, long/native UTF-16 names, a disabled/unavailable
Recycle Bin, capacity exhaustion, removable media, denied access, and namespace
handlers that do not implement recycling. A method unsupported by a particular
namespace may fail even when Explorer offers a different recycling route; that
failure is deliberately preserved.

Relevant system contracts:

- [ITransferSource::RecycleItem](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-itransfersource-recycleitem)
- [ITransferSource methods, including distinct RemoveItem](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-itransfersource)
- [IShellItem::BindToHandler](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ishellitem-bindtohandler)
- [SHGetKnownFolderIDList](https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderidlist)
- [ITransferAdviseSink::FileFailure](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-itransferadvisesink-filefailure)
- [IFileOperation::SetOperationFlags](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ifileoperation-setoperationflags)

## Rust-only cross-check without a Windows C SDK

The normal Windows build still uses bundled SQLite and must be built/tested on
Windows. For this **check-only** invocation, `libsqlite3-sys`'s documented build
script environment branch bypasses compiling SQLite C and reuses its bundled
Rust bindings. An empty library directory makes it explicit that no Windows
SQLite binary is supplied. `cargo check` does not link it.

```sh
mkdir -p /tmp/ohmydisk-windows-check-native
LIBSQLITE3_SYS_USE_PKG_CONFIG=1 \
SQLITE3_LIB_DIR=/tmp/ohmydisk-windows-check-native \
SQLITE3_INCLUDE_DIR=/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/usr/include \
PKG_CONFIG_ALLOW_CROSS=1 \
SQLITE3_NO_PKG_CONFIG=1 \
cargo check --workspace --all-targets --target x86_64-pc-windows-msvc
```

These variables are not saved to repository configuration. The include directory
is a local header location; no host SQLite library is linked into a Windows
binary. SQLite C compilation, its Windows ABI, the Windows linker, COM behavior,
and GUI/IPC runtime behavior remain the responsibility of Windows CI or a real
Windows machine.
