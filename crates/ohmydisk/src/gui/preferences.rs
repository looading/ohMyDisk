//! Small GUI preferences, independent of scan snapshots and the MCP service.
use super::locale::Language;
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
    version: u64,
    saved_version: u64,
}

impl Writer {
    pub fn new(data_dir: PathBuf, language: Language) -> Self {
        Self(Arc::new(WriterState {
            data_dir,
            selection: Mutex::new(Selection {
                language,
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

    pub fn has_unsaved(&self) -> bool {
        let selection = self.selection();
        selection.version != selection.saved_version
    }

    pub fn flush(&self) -> Result<(), String> {
        self.flush_with(|language| save(&self.0.data_dir, language))
    }

    fn flush_with(
        &self,
        mut write: impl FnMut(Language) -> Result<(), String>,
    ) -> Result<(), String> {
        let _writing = self
            .0
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let (language, version) = {
                let selection = self.selection();
                if selection.version == selection.saved_version {
                    return Ok(());
                }
                (selection.language, selection.version)
            };
            // Never hold the selection lock during filesystem I/O: changing
            // the language stays responsive even while an earlier write waits.
            write(language)?;
            self.selection().saved_version = version;
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Preferences {
    language: Language,
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

pub(super) fn save(data_dir: &Path, language: Language) -> Result<(), String> {
    ohmydisk_platform::prepare_private_dir(data_dir).map_err(|error| error.to_string())?;
    // Preserve future settings. Explicitly selecting a language can also repair
    // a malformed preferences document after startup has reported the error.
    let mut preferences = read(data_dir).unwrap_or_default();
    preferences.language = language;
    let bytes = serde_json::to_vec_pretty(&preferences).map_err(|error| error.to_string())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(data_dir).map_err(|error| error.to_string())?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.write_all(b"\n"))
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
        let (entered_send, entered) = std::sync::mpsc::channel();
        let (release, release_receive) = std::sync::mpsc::channel();
        let worker = writer.clone();
        let worker_dir = data_dir.clone();
        let saving = std::thread::spawn(move || {
            let mut first = true;
            worker.flush_with(|language| {
                if first {
                    first = false;
                    entered_send.send(()).unwrap();
                    release_receive.recv().unwrap();
                }
                save(&worker_dir, language)
            })
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // Selecting another language must not wait on the blocked disk writer.
        writer.select(Language::English);
        assert!(writer.has_unsaved());
        let closing = writer.clone();
        let draining = std::thread::spawn(move || closing.flush());
        release.send(()).unwrap();
        saving.join().unwrap().unwrap();
        draining.join().unwrap().unwrap();
        assert!(!writer.has_unsaved());
        assert_eq!(load(&data_dir).unwrap(), Language::English);
    }
}
