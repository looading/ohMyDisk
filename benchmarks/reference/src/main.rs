//! Reconstructed diagnostic wrapper for the pinned published v0.1.0 scanner.
//! This is not the unavailable original uncommitted baseline runner source.
//! Fixture creation, serialization, hashing and queries are outside scan_ms.
use ohmydisk_core::{Platform, model::*};
use ohmydisk_platform::NativePlatform;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

struct MeasuredPlatform {
    inner: NativePlatform,
    calls: AtomicU64,
    nanos: AtomicU64,
}
impl Platform for MeasuredPlatform {
    fn metadata(&self, path: &Path) -> io::Result<FileInfo> {
        let start = Instant::now();
        let result = self.inner.metadata(path);
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.nanos.fetch_add(
            start.elapsed().as_nanos().try_into().unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        result
    }
    fn volumes(&self) -> io::Result<Vec<Volume>> {
        self.inner.volumes()
    }
    fn trash(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::other("Benchmark never cleans files"))
    }
    fn reveal(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::other("Benchmark never opens files"))
    }
    fn protected_paths(&self) -> Vec<PathBuf> {
        self.inner.protected_paths()
    }
    fn protected_subtrees(&self) -> Vec<PathBuf> {
        self.inner.protected_subtrees()
    }
}

fn relative(path: &NativePath, root: &Path) -> Result<String, AppError> {
    let native = path.to_path()?;
    let relative = native
        .strip_prefix(root)
        .map_err(|_| AppError::new("invalid_snapshot", "Node outside root"))?;
    Ok(NativePath::new(if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    })
    .encoded)
}

fn digest(snapshot: &Snapshot) -> Result<String, Box<dyn std::error::Error>> {
    let root = snapshot.root_path.to_path()?;
    let paths = snapshot
        .nodes
        .iter()
        .map(|n| Ok((n.id.as_str(), relative(&n.path, &root)?)))
        .collect::<Result<HashMap<_, _>, AppError>>()?;
    let mut nodes = snapshot
        .nodes
        .iter()
        .map(|n| {
            (
                paths[n.id.as_str()].clone(),
                n.kind,
                n.parent_id.as_deref().and_then(|p| paths.get(p)).cloned(),
                n.logical_bytes,
                n.allocated_bytes,
                n.complete,
                n.is_link_or_reparse,
                n.link_count,
            )
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|a, b| a.0.cmp(&b.0));
    let mut issues = snapshot
        .issues
        .iter()
        .map(|i| {
            let path = Path::new(&i.path)
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "<root>".into());
            (path, i.code.clone())
        })
        .collect::<Vec<_>>();
    issues.sort();
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            nodes,
            issues,
            snapshot.complete,
            snapshot.cancelled
        ))?)
    ))
}

#[cfg(unix)]
fn peak_rss() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage initializes the supplied valid rusage on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: successful getrusage initialized all fields.
    let value = u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).ok()?;
    #[cfg(target_os = "macos")]
    {
        Some(value)
    }
    #[cfg(not(target_os = "macos"))]
    {
        value.checked_mul(1024)
    }
}
#[cfg(not(unix))]
fn peak_rss() -> Option<u64> {
    None
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = PathBuf::from(
        arguments
            .next()
            .ok_or("Expected root --output JSON [--cancel-after-ms N]")?,
    );
    let mut output = None;
    let mut cancel_after = None;
    while let Some(flag) = arguments.next() {
        match flag.to_str() {
            Some("--output") => {
                output = Some(PathBuf::from(
                    arguments.next().ok_or("Expected output file")?,
                ))
            }
            Some("--cancel-after-ms") => {
                cancel_after = Some(
                    arguments
                        .next()
                        .ok_or("Expected duration")?
                        .to_str()
                        .ok_or("Invalid duration")?
                        .parse::<u64>()?,
                )
            }
            _ => return Err("Unknown benchmark option".into()),
        }
    }
    let output = output.ok_or("Expected --output")?;
    let platform = MeasuredPlatform {
        inner: NativePlatform,
        calls: AtomicU64::new(0),
        nanos: AtomicU64::new(0),
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let requested = Arc::new(Mutex::new(None::<Instant>));
    let (stop, receiver) = std::sync::mpsc::channel::<()>();
    let timer = cancel_after.map(|ms| {
        let flag = cancelled.clone();
        let requested = requested.clone();
        std::thread::spawn(move || {
            if receiver.recv_timeout(Duration::from_millis(ms)).is_err() {
                *requested.lock().unwrap() = Some(Instant::now());
                flag.store(true, Ordering::Release);
            }
        })
    });
    let started = Instant::now();
    let mut callbacks = 0u64;
    let snapshot =
        ohmydisk_core::scan::scan(&root, &platform, &cancelled, &mut |_| callbacks += 1)?;
    let ended = Instant::now();
    let scan_ms = ended.duration_since(started).as_secs_f64() * 1000.0;
    let peak_rss_bytes = peak_rss();
    let _ = stop.send(());
    if let Some(timer) = timer {
        timer.join().map_err(|_| "Cancel timer panicked")?;
    }
    let cancel_latency_ms = if snapshot.cancelled {
        requested
            .lock()
            .unwrap()
            .map(|request| ended.saturating_duration_since(request).as_secs_f64() * 1000.0)
    } else {
        None
    };
    let serialization = Instant::now();
    let encoded = serde_json::to_vec(&snapshot)?;
    let serialize_ms = serialization.elapsed().as_secs_f64() * 1000.0;
    let snapshot_bytes = encoded.len();
    drop(encoded);
    let query_start = Instant::now();
    let _children = if snapshot.nodes.is_empty() {
        None
    } else {
        Some(ohmydisk_core::query::query(
            &snapshot,
            &Query {
                limit: Some(100),
                ..Default::default()
            },
        )?)
    };
    let first_children_ms = query_start.elapsed().as_secs_f64() * 1000.0;
    let query_start = Instant::now();
    let _descendants = if snapshot.nodes.is_empty() {
        None
    } else {
        Some(ohmydisk_core::query::query(
            &snapshot,
            &Query {
                descendants: true,
                limit: Some(500),
                ..Default::default()
            },
        )?)
    };
    let first_descendants_ms = query_start.elapsed().as_secs_f64() * 1000.0;
    let report = serde_json::json!({
        "scan_ms":scan_ms,"serialize_ms":serialize_ms,"snapshot_bytes":snapshot_bytes,"nodes":snapshot.nodes.len(),
        "files":snapshot.nodes.iter().filter(|n|n.kind!=NodeKind::Directory).count(),
        "directories":snapshot.nodes.iter().filter(|n|n.kind==NodeKind::Directory&&!n.is_link_or_reparse).count(),
        "logical_bytes":snapshot.nodes.first().map_or(0,|n|n.logical_bytes),"complete":snapshot.complete,"cancelled":snapshot.cancelled,
        "metadata_calls":platform.calls.load(Ordering::Relaxed),"metadata_ms":platform.nanos.load(Ordering::Relaxed) as f64/1_000_000.0,
        "progress_callbacks":callbacks,"peak_rss_bytes":peak_rss_bytes,"cancel_latency_ms":cancel_latency_ms,
        "first_children_ms":first_children_ms,"first_descendants_ms":first_descendants_ms,"issue_count":snapshot.issues.len(),
        "validation_digest":digest(&snapshot)?,"digest_version":1
    });
    std::fs::write(output, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
fn main() {
    if run().is_err() {
        eprintln!("Benchmark failed");
        std::process::exit(1);
    }
}
