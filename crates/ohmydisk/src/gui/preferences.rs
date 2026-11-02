//! Small GUI preferences, independent of scan snapshots and the MCP service.
use super::locale::Language;
use ohmydisk_core::NativePath;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

const FILE_NAME: &str = "preferences.json";
const MAX_BYTES: u64 = 65_536;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct FolderSettings {
    pub roots: Vec<NativePath>,
    pub removed: Vec<NativePath>,
}

/// Selection changes are in-memory only. A blocking worker serializes writes
/// and drains the latest selection independently of GUI completion messages.
#[derive(Clone)]
pub(super) struct Writer(Arc<WriterState>);

struct WriterState {
    data_dir: PathBuf,
    selection: Mutex<Selection>,
    writing: Mutex<()>,
}

struct Selection {
    language: Language,
    folders: FolderSettings,
    version: u64,
    saved_version: u64,
}

impl Writer {
    pub fn new(data_dir: PathBuf, language: Language) -> Self {
        let folders = load_folders(&data_dir).unwrap_or_default();
        Self(Arc::new(WriterState {
            data_dir,
            selection: Mutex::new(Selection {
                language,
                folders,
                version: 0,
                saved_version: 0,
            }),
            writing: Mutex::new(()),
        }))
    }

    fn selection(&self) -> MutexGuard<'_, Selection> {
        self.0
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn select(&self, language: Language) {
        let mut selection = self.selection();
        if selection.language != language {
            selection.language = language;
            selection.version = selection.version.wrapping_add(1);
        }
    }

    pub fn set_folders(&self, folders: FolderSettings) {
        let mut selection = self.selection();
        if selection.folders != folders {
            selection.folders = folders;
            selection.version = selection.version.wrapping_add(1);
        }
    }

    pub fn has_unsaved(&self) -> bool {
        let selection = self.selection();
        selection.version != selection.saved_version
    }

    pub fn flush(&self) -> Result<(), String> {
        self.flush_with(|language, folders| {
            save_settings(&self.0.data_dir, language, Some(folders))
        })
    }

    fn flush_with(
        &self,
        mut write: impl FnMut(Language, &FolderSettings) -> Result<(), String>,
    ) -> Result<(), String> {
        let _writing = self
            .0
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let (language, folders, version) = {
                let selection = self.selection();
                if selection.version == selection.saved_version {
                    return Ok(());
                }
                (
                    selection.language,
                    selection.folders.clone(),
                    selection.version,
                )
            };
            // Never hold the selection lock during filesystem I/O: language
            // and folder changes stay responsive while a write waits.
            write(language, &folders)?;
            self.selection().saved_version = version;
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Preferences {
    language: Language,
    folders: FolderSettings,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

fn read(data_dir: &Path) -> Result<Preferences, String> {
    let path = data_dir.join(FILE_NAME);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Preferences::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Preferences must be a regular file".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Preferences must not be a reparse point".into());
        }
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err("Preferences exceed 64 KiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

pub(super) fn load(data_dir: &Path) -> Result<Language, String> {
    read(data_dir).map(|preferences| preferences.language)
}

pub(super) fn load_folders(data_dir: &Path) -> Result<FolderSettings, String> {
    read(data_dir).map(|preferences| preferences.folders)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn save(data_dir: &Path, language: Language) -> Result<(), String> {
    save_settings(data_dir, language, None)
}

fn save_settings(
    data_dir: &Path,
    language: Language,
    folders: Option<&FolderSettings>,
) -> Result<(), String> {
    ohmydisk_platform::prepare_private_dir(data_dir).map_err(|error| error.to_string())?;
    // Preserve future settings. Explicitly selecting a language can also repair
    // a malformed preferences document after startup has reported the error.
    let mut preferences = read(data_dir).unwrap_or_default();
    preferences.language = language;
    if let Some(folders) = folders {
        preferences.folders = folders.clone();
    }
    let mut bytes = serde_json::to_vec_pretty(&preferences).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err("Preferences exceed 64 KiB".into());
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(data_dir).map_err(|error| error.to_string())?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temporary
        .persist(data_dir.join(FILE_NAME))
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_is_english_without_creating_a_file() {
        let temporary = tempfile::tempdir().unwrap();
        let absent = temporary.path().join("absent");
        assert_eq!(load(&absent).unwrap(), Language::English);
        assert_eq!(load_folders(&absent).unwrap(), FolderSettings::default());
        assert!(!absent.exists());
    }

    #[test]
    fn language_survives_reopen_and_replacement_preserves_other_settings() {
        let temporary = tempfile::tempdir().unwrap();
        let data_dir = temporary.path().join("state");
        save(&data_dir, Language::Chinese).unwrap();
        assert_eq!(load(&data_dir).unwrap(), Language::Chinese);
        fs::write(
            data_dir.join(FILE_NAME),
            br#"{"language":"zh-CN","future_setting":42}"#,
        )
        .unwrap();
        save(&data_dir, Language::English).unwrap();
        assert_eq!(load(&data_dir).unwrap(), Language::English);
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(data_dir.join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(value["future_setting"], 42);
        assert_eq!(value["language"], "en");
        assert_eq!(fs::read_dir(&data_dir).unwrap().count(), 1);
    }

    #[test]
    fn malformed_or_unknown_language_reports_an_error_and_explicit_save_repairs_it() {
        let temporary = tempfile::tempdir().unwrap();
        let data_dir = temporary.path().join("state");
        ohmydisk_platform::prepare_private_dir(&data_dir).unwrap();
        for invalid in [b"{".as_slice(), br#"{"language":"unknown"}"#.as_slice()] {
            fs::write(data_dir.join(FILE_NAME), invalid).unwrap();
            assert!(load(&data_dir).is_err());
            save(&data_dir, Language::Chinese).unwrap();
            assert_eq!(load(&data_dir).unwrap(), Language::Chinese);
        }
        fs::write(data_dir.join(FILE_NAME), vec![b' '; 65_537]).unwrap();
        assert!(load(&data_dir).is_err());
    }

    #[test]
    fn failed_save_returns_an_error_without_partial_configuration() {
        let temporary = tempfile::tempdir().unwrap();
        let data_dir = temporary.path().join("state");
        ohmydisk_platform::prepare_private_dir(&data_dir).unwrap();
        fs::create_dir(data_dir.join(FILE_NAME)).unwrap();
        assert!(save(&data_dir, Language::Chinese).is_err());
        assert!(data_dir.join(FILE_NAME).is_dir());
        assert_eq!(fs::read_dir(&data_dir).unwrap().count(), 1);
    }

    #[test]
    fn closing_during_a_blocked_write_drains_the_latest_selection_without_gui_events() {
        let temporary = tempfile::tempdir().unwrap();
        let data_dir = temporary.path().join("state");
        let writer = Writer::new(data_dir.clone(), Language::English);
        writer.select(Language::Chinese);
        let first_root = NativePath::new(&temporary.path().join("first"));
        writer.set_folders(FolderSettings {
            roots: vec![first_root.clone()],
            removed: Vec::new(),
        });
        let (entered_send, entered) = std::sync::mpsc::channel();
        let (release, release_receive) = std::sync::mpsc::channel();
        let worker = writer.clone();
        let worker_dir = data_dir.clone();
        let saving = std::thread::spawn(move || {
            let mut first = true;
            worker.flush_with(|language, folders| {
                if first {
                    first = false;
                    entered_send.send(()).unwrap();
                    release_receive.recv().unwrap();
                }
                save_settings(&worker_dir, language, Some(folders))
            })
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // Selecting another language must not wait on the blocked disk writer.
        writer.select(Language::English);
        let latest_folders = FolderSettings {
            roots: vec![NativePath::new(&temporary.path().join("latest"))],
            removed: vec![first_root],
        };
        writer.set_folders(latest_folders.clone());
        assert!(writer.has_unsaved());
        let closing = writer.clone();
        let draining = std::thread::spawn(move || closing.flush());
        release.send(()).unwrap();
        saving.join().unwrap().unwrap();
        draining.join().unwrap().unwrap();
        assert!(!writer.has_unsaved());
        assert_eq!(load(&data_dir).unwrap(), Language::English);
        assert_eq!(load_folders(&data_dir).unwrap(), latest_folders);
    }

    #[test]
    fn folder_registration_removal_and_readdition_survive_reopen() {
        let temporary = tempfile::tempdir().unwrap();
        let data_dir = temporary.path().join("state");
        let folder = NativePath::new(&temporary.path().join("项目"));
        let writer = Writer::new(data_dir.clone(), Language::English);
        writer.set_folders(FolderSettings {
            roots: vec![folder.clone()],
            removed: Vec::new(),
        });
        writer.flush().unwrap();
        assert_eq!(load_folders(&data_dir).unwrap().roots, vec![folder.clone()]);

        let removed = FolderSettings {
            roots: Vec::new(),
            removed: vec![folder.clone()],
        };
        writer.set_folders(removed.clone());
        writer.flush().unwrap();
        assert_eq!(load_folders(&data_dir).unwrap(), removed);

        // Reopening and changing only the language retains the removal marker.
        let reopened = Writer::new(data_dir.clone(), Language::English);
        reopened.select(Language::Chinese);
        reopened.flush().unwrap();
        assert_eq!(load(&data_dir).unwrap(), Language::Chinese);
        assert_eq!(load_folders(&data_dir).unwrap(), removed);
        save(&data_dir, Language::English).unwrap();
        assert_eq!(load_folders(&data_dir).unwrap(), removed);

        let readded = FolderSettings {
            roots: vec![folder],
            removed: Vec::new(),
        };
        reopened.set_folders(readded.clone());
        reopened.flush().unwrap();
        assert_eq!(load_folders(&data_dir).unwrap(), readded);
    }

    #[test]
    fn folder_writes_preserve_unknown_settings_and_the_selected_language() {
        let temporary = tempfile::tempdir().unwrap();
        let data_dir = temporary.path().join("state");
        ohmydisk_platform::prepare_private_dir(&data_dir).unwrap();
        fs::write(
            data_dir.join(FILE_NAME),
            br#"{"language":"zh-CN","future_setting":{"enabled":true}}"#,
        )
        .unwrap();
        let writer = Writer::new(data_dir.clone(), load(&data_dir).unwrap());
        let folders = FolderSettings {
            roots: vec![NativePath::new(&temporary.path().join("custom"))],
            removed: Vec::new(),
        };
        writer.set_folders(folders.clone());
        writer.flush().unwrap();
        assert_eq!(load(&data_dir).unwrap(), Language::Chinese);
        assert_eq!(load_folders(&data_dir).unwrap(), folders);
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(data_dir.join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(value["future_setting"]["enabled"], true);
    }

    #[test]
    fn oversized_folder_write_keeps_the_last_readable_configuration() {
        let temporary = tempfile::tempdir().unwrap();
        let data_dir = temporary.path().join("state");
        save(&data_dir, Language::English).unwrap();
        let original = fs::read(data_dir.join(FILE_NAME)).unwrap();
        let writer = Writer::new(data_dir.clone(), Language::English);
        writer.select(Language::Chinese);
        writer.set_folders(FolderSettings {
            roots: vec![NativePath {
                display: "x".repeat(65_536),
                encoded: String::new(),
            }],
            removed: Vec::new(),
        });

        assert!(writer.flush().unwrap_err().contains("64 KiB"));
        assert!(writer.has_unsaved());
        assert_eq!(fs::read(data_dir.join(FILE_NAME)).unwrap(), original);
        assert_eq!(load(&data_dir).unwrap(), Language::English);
        assert_eq!(load_folders(&data_dir).unwrap(), FolderSettings::default());
        assert_eq!(fs::read_dir(&data_dir).unwrap().count(), 1);
    }

    #[test]
    fn language_helper_rejects_serialization_that_would_exceed_the_read_limit() {
        let temporary = tempfile::tempdir().unwrap();
        let data_dir = temporary.path().join("state");
        ohmydisk_platform::prepare_private_dir(&data_dir).unwrap();
        let original = format!(r#"{{"language":"en","future":"{}"}}"#, "x".repeat(65_500));
        assert!(u64::try_from(original.len()).unwrap() <= MAX_BYTES);
        fs::write(data_dir.join(FILE_NAME), &original).unwrap();
        assert_eq!(load(&data_dir).unwrap(), Language::English);

        // Adding the default folders and pretty-printing would cross 64 KiB.
        assert!(
            save(&data_dir, Language::Chinese)
                .unwrap_err()
                .contains("64 KiB")
        );
        assert_eq!(
            fs::read_to_string(data_dir.join(FILE_NAME)).unwrap(),
            original
        );
        assert_eq!(load(&data_dir).unwrap(), Language::English);
    }
}
