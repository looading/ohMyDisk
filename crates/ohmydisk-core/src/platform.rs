use crate::{FileInfo, Volume};
use std::{
    io,
    path::{Path, PathBuf},
};

/// An owned directory item; optional metadata can come from a native bulk API.
#[derive(Debug)]
pub struct ScanEntry {
    pub path: PathBuf,
    pub info: Option<io::Result<FileInfo>>,
}

#[derive(Debug)]
pub enum DirectoryEvent {
    Entry(ScanEntry),
    Error(io::Error),
}

/// Portable, streaming enumeration. Returning false stops enumeration promptly.
pub fn read_directory_portable(
    path: &Path,
    visitor: &mut dyn FnMut(DirectoryEvent) -> bool,
) -> io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let event = match entry {
            Ok(entry) => DirectoryEvent::Entry(ScanEntry {
                path: entry.path(),
                info: None,
            }),
            Err(error) => DirectoryEvent::Error(error),
        };
        if !visitor(event) {
            break;
        }
    }
    Ok(())
}

pub trait Platform: Send + Sync {
    fn metadata(&self, path: &Path) -> io::Result<FileInfo>;
    fn volumes(&self) -> io::Result<Vec<Volume>>;
    fn trash(&self, path: &Path) -> io::Result<()>;
    fn reveal(&self, path: &Path) -> io::Result<()>;
    fn protected_paths(&self) -> Vec<PathBuf>;
    fn read_directory(
        &self,
        path: &Path,
        visitor: &mut dyn FnMut(DirectoryEvent) -> bool,
    ) -> io::Result<()> {
        read_directory_portable(path, visitor)
    }
    /// Entire trees that must never be cleanup targets (unlike a home directory,
    /// whose children remain user-selectable).
    fn protected_subtrees(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}
