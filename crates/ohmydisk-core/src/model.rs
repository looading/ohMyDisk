use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct AppError {
    pub code: String,
    pub message: String,
}
impl AppError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::new(
            match e.kind() {
                std::io::ErrorKind::PermissionDenied => "permission_denied",
                std::io::ErrorKind::NotFound => "not_found",
                _ => "io_error",
            },
            e.to_string(),
        )
    }
}

/// Display is never used for filesystem operations. Encoded retains native path units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativePath {
    pub display: String,
    pub encoded: String,
}
impl NativePath {
    pub fn new(path: &Path) -> Self {
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            path.as_os_str().as_bytes().to_vec()
        };
        #[cfg(windows)]
        let bytes = {
            use std::os::windows::ffi::OsStrExt;
            path.as_os_str()
                .encode_wide()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        };
        Self {
            display: path.to_string_lossy().into_owned(),
            encoded: STANDARD.encode(bytes),
        }
    }
    pub fn to_path(&self) -> Result<PathBuf, AppError> {
        let bytes = STANDARD
            .decode(&self.encoded)
            .map_err(|e| AppError::new("invalid_path", e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            Ok(std::ffi::OsString::from_vec(bytes).into())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            if bytes.len() % 2 != 0 {
                return Err(AppError::new(
                    "invalid_path",
                    "Invalid UTF-16 path encoding",
                ));
            }
            let wide = bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>();
            Ok(std::ffi::OsString::from_wide(&wide).into())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_bytes: Option<u64>,
    pub kind: NodeKind,
    pub logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
    pub modified_ns: Option<u64>,
    pub identity: String,
    pub volume_id: String,
    pub link_count: u64,
    pub is_link_or_reparse: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_observation: Option<crate::SpaceObservation>,
    pub id: String,
    pub name: String,
    pub mount_path: NativePath,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_usage: Option<crate::NodeSpace>,
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub path: NativePath,
    pub kind: NodeKind,
    pub logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
    pub modified_ns: Option<u64>,
    pub identity: String,
    pub volume_id: String,
    pub link_count: u64,
    pub complete: bool,
    pub is_link_or_reparse: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanIssue {
    pub node_id: Option<String>,
    pub path: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanProgress {
    pub files: u64,
    pub directories: u64,
    pub bytes: u64,
    pub current_path: String,
    pub phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_accounting: Option<crate::SpaceAccounting>,
    pub id: String,
    pub scan_id: String,
    pub root_id: String,
    pub root_path: NativePath,
    pub created_at: u64,
    pub complete: bool,
    pub cancelled: bool,
    pub nodes: Vec<Node>,
    pub issues: Vec<ScanIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupMode {
    Trash,
    Permanent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTarget {
    pub node: Node,
    pub members: Vec<Node>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupPlan {
    pub id: String,
    pub scan_id: String,
    pub snapshot_id: String,
    pub hash: String,
    pub mode: CleanupMode,
    pub created_at: u64,
    pub expires_at: u64,
    pub logical_bytes: u64,
    pub targets: Vec<PlanTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Started,
    Succeeded,
    Failed,
    Skipped,
    NotStarted,
    InDoubt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemResult {
    pub node_id: String,
    pub path: String,
    pub status: ItemStatus,
    pub code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub created_at: u64,
    /// Monotonic scan duration, including queueing, saving, and publication.
    /// Active jobs report a live value; terminal jobs retain their saved total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    pub progress: ScanProgress,
    pub result: serde_json::Value,
    pub error: Option<AppError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Query {
    #[serde(default, skip_serializing_if = "crate::SizeMetric::is_logical")]
    pub size_metric: crate::SizeMetric,
    pub scope_node_id: Option<String>,
    pub descendants: bool,
    pub name: Option<String>,
    pub kind: Option<NodeKind>,
    pub min_bytes: Option<u64>,
    pub max_bytes: Option<u64>,
    pub modified_before: Option<u64>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[cfg(test)]
mod job_timing_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_jobs_keep_unknown_timing_and_new_totals_round_trip() {
        let mut job: Job = serde_json::from_value(json!({
            "id": "legacy", "kind": "scan", "state": "completed",
            "created_at": 100, "progress": ScanProgress::default(),
            "result": {}, "error": null,
        }))
        .unwrap();
        assert_eq!(job.elapsed_ms, None);
        assert_eq!(job.finished_at, None);
        let legacy = serde_json::to_value(&job).unwrap();
        assert!(legacy.get("elapsed_ms").is_none());
        assert!(legacy.get("finished_at").is_none());

        job.elapsed_ms = Some(12_345);
        job.finished_at = Some(12_445);
        let saved: Job = serde_json::from_str(&serde_json::to_string(&job).unwrap()).unwrap();
        assert_eq!(saved.elapsed_ms, Some(12_345));
        assert_eq!(saved.finished_at, Some(12_445));
    }
}
