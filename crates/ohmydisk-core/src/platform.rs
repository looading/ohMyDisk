use crate::{FileInfo, Volume};
use std::{
    io,
    path::{Path, PathBuf},
};
pub trait Platform: Send + Sync {
    fn metadata(&self, path: &Path) -> io::Result<FileInfo>;
    fn volumes(&self) -> io::Result<Vec<Volume>>;
    fn trash(&self, path: &Path) -> io::Result<()>;
    fn reveal(&self, path: &Path) -> io::Result<()>;
    fn protected_paths(&self) -> Vec<PathBuf>;
    /// Entire trees that must never be cleanup targets (unlike a home directory,
    /// whose children remain user-selectable).
    fn protected_subtrees(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}
