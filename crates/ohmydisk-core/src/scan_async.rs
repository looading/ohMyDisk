//! Bounded directory workers with one snapshot reducer. The snapshot and pending
//! directory frontier grow with the tree; worker count and transport batches do not.
use crate::scan::{ScanBuilder, matching_observation, prepare_root};
use crate::{
    AppError, DirectoryEvent, FileInfo, NodeKind, Platform, ScanEntry, ScanProgress, Snapshot,
};
use std::{
    collections::HashMap,
    io,
    mem::size_of,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
};

#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    pub workers: usize,
    pub batch_entries: usize,
    pub batch_bytes: usize,
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            workers: 4,
            batch_entries: 128,
            batch_bytes: 256 * 1024,
        }
    }
}
impl ScanOptions {
    fn validate(self) -> Result<Self, AppError> {
        if !(1..=64).contains(&self.workers)
            || !(1..=4096).contains(&self.batch_entries)
            || !(1..=16 * 1024 * 1024).contains(&self.batch_bytes)
        {
            return Err(AppError::new(
                "invalid_arguments",
                "Scan worker or batch limits are outside their supported range",
            ));
        }
        Ok(self)
    }
}

struct Work {
    path: PathBuf,
    parent: Option<usize>,
}
enum Item {
    Entry(PathBuf, io::Result<FileInfo>),
    Directory(PathBuf),
    Error(io::Error),
}
enum Event {
    Admission {
        work: Work,
        info: io::Result<FileInfo>,
        reply: oneshot::Sender<Option<usize>>,
    },
    Batch {
        parent: usize,
        items: Vec<Item>,
    },
    DirectoryDone {
        parent: usize,
        result: io::Result<()>,
    },
}

fn stopped(cancelled: &AtomicBool, stop: &AtomicBool) -> bool {
    cancelled.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed)
}
fn worker_error(error: impl std::fmt::Display) -> AppError {
    AppError::new("worker_panic", error.to_string())
}

struct StopOnDrop(Arc<AtomicBool>);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Scan blocking filesystem APIs concurrently without blocking runtime threads.
/// One filesystem permit covers fresh metadata, admission, and enumeration.
/// The reducer never waits for this budget or a full dispatch queue, so a
/// worker waiting for admission cannot form a permit dependency cycle.
/// Cancellation is cooperative: all started blocking workers finish before return.
/// Dropping this future also requests cooperative shutdown; callers that require a
/// join guarantee must cancel through `cancelled` and await the result.
///
/// # Errors
/// Returns invalid root/options errors, a closed worker-budget error, or
/// `worker_panic` after stopping and joining all other directory workers.
pub async fn scan_async(
    root: PathBuf,
    platform: Arc<dyn Platform>,
    cancelled: Arc<AtomicBool>,
    options: ScanOptions,
    filesystem_budget: Arc<Semaphore>,
    progress: watch::Sender<ScanProgress>,
) -> Result<Snapshot, AppError> {
    let options = options.validate()?;
    let stop = Arc::new(AtomicBool::new(false));
    // Install before preparation, including its potentially queued permit wait.
    let _stop_on_drop = StopOnDrop(stop.clone());
    let prepared = prepare_async(
        root.clone(),
        platform.clone(),
        filesystem_budget.clone(),
        cancelled.clone(),
        stop.clone(),
    )
    .await?;
    let Some((root, volume, before)) = prepared else {
        return Ok(cancelled_snapshot(&root, &cancelled, &progress));
    };
    let mut builder = ScanBuilder::new(&root, volume, before);
    progress.send_replace(builder.counts.clone());
    let (sender, mut receiver) = mpsc::channel(options.workers.saturating_mul(2));
    let mut workers = JoinSet::new();
    let mut ready = vec![Work {
        path: root.clone(),
        parent: None,
    }];
    let mut children: HashMap<usize, Vec<Work>> = HashMap::new();
    let mut last_progress = Instant::now();
    let mut failure = None;
    let mut closing = false;
    loop {
        if stopped(&cancelled, &stop) && !closing {
            closing = true;
            stop.store(true, Ordering::Relaxed);
            ready.clear();
            children.clear();
            // Dropping queued admissions disconnects every oneshot waiter. A
            // closed receiver also releases blocking_send backpressure waits.
            receiver.close();
            while receiver.try_recv().is_ok() {}
        }
        while !closing && workers.len() < options.workers {
            let Some(work) = ready.pop() else {
                break;
            };
            workers.spawn(run_directory(
                work,
                platform.clone(),
                cancelled.clone(),
                stop.clone(),
                options,
                filesystem_budget.clone(),
                sender.clone(),
            ));
        }
        if workers.is_empty() && (closing || (ready.is_empty() && receiver.is_empty())) {
            break;
        }
        tokio::select! {
            event = receiver.recv(), if !closing => {
                if let Some(event) = event {
                    apply_event(event, &mut builder, &mut ready, &mut children, &cancelled, &stop);
                    if last_progress.elapsed() >= Duration::from_millis(250) {
                        progress.send_replace(builder.counts.clone());
                        last_progress = Instant::now();
                    }
                }
            }
            joined = workers.join_next(), if !workers.is_empty() => {
                let error = match joined {
                    Some(Ok(Err(error))) => Some(error),
                    Some(Err(error)) => Some(worker_error(error)),
                    _ => None,
                };
                if let Some(error) = error {
                    failure.get_or_insert(error);
                    stop.store(true, Ordering::Relaxed);
                }
            }
            // Cancellation is an AtomicBool shared with sync filesystem code;
            // poll it even if all workers are inside a slow system call.
            () = tokio::time::sleep(Duration::from_millis(20)), if !closing => {}
        }
    }
    drop(receiver);
    // All normal paths above consume every JoinSet entry. No abort is used:
    // aborting spawn_blocking would leave an active filesystem worker behind.
    if let Some(error) = failure {
        return Err(error);
    }
    let after = observe_after(
        root,
        platform,
        builder.volume.clone(),
        filesystem_budget,
        cancelled.clone(),
        stop.clone(),
    )
    .await?;
    let cancelled_at_finish = cancelled.clone();
    let (snapshot, counts) =
        tokio::task::spawn_blocking(move || builder.finish(&cancelled_at_finish, after))
            .await
            .map_err(worker_error)?;
    progress.send_replace(counts);
    Ok(snapshot)
}

async fn run_directory(
    work: Work,
    platform: Arc<dyn Platform>,
    cancelled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    options: ScanOptions,
    budget: Arc<Semaphore>,
    sender: mpsc::Sender<Event>,
) -> Result<(), AppError> {
    let Some(permit) = acquire_budget(budget, &cancelled, &stop).await? else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || {
        // One blocking task and one filesystem permit cover the whole directory.
        // The reducer drains admissions/results without acquiring this budget,
        // so blocking here for its decision cannot form a permit wait cycle.
        if stopped(&cancelled, &stop) {
            return;
        }
        let path = work.path.clone();
        let metadata = platform.scan_metadata(&path);
        if stopped(&cancelled, &stop) {
            return;
        }
        let (reply, answer) = oneshot::channel();
        if sender
            .blocking_send(Event::Admission {
                work,
                info: metadata,
                reply,
            })
            .is_err()
        {
            return;
        }
        let Ok(Some(parent)) = answer.blocking_recv() else {
            return;
        };
        if stopped(&cancelled, &stop) {
            return;
        }
        let batch_capacity = options
            .batch_entries
            .min((options.batch_bytes / size_of::<Item>()).max(1));
        let mut batch = Vec::new();
        let mut bytes = 0_usize;
        let mut channel_open = true;
        let result = platform.read_directory(&path, &mut |event| {
            if stopped(&cancelled, &stop) || !channel_open {
                return false;
            }
            let item = match event {
                DirectoryEvent::Error(error) => Item::Error(error),
                DirectoryEvent::Entry(entry) => resolve_entry(entry, platform.as_ref()),
            };
            let entry_bytes = item_bytes(&item);
            if !batch.is_empty()
                && (batch.len() >= options.batch_entries
                    || bytes.saturating_add(entry_bytes) > options.batch_bytes)
            {
                channel_open = send_batch(&sender, parent, &mut batch);
                bytes = 0;
                if !channel_open || stopped(&cancelled, &stop) {
                    return false;
                }
            }
            if batch.capacity() == 0 {
                // mem::take transfers the previous allocation to the reducer.
                // Reserve once for its replacement, only if another item exists.
                // Empty directories and final flushes allocate no spare batch.
                batch.reserve_exact(batch_capacity);
            }
            bytes = bytes.saturating_add(entry_bytes);
            batch.push(item);
            if batch.len() >= options.batch_entries || bytes >= options.batch_bytes {
                channel_open = send_batch(&sender, parent, &mut batch);
                bytes = 0;
            }
            channel_open && !stopped(&cancelled, &stop)
        });
        if channel_open && !stopped(&cancelled, &stop) {
            send_batch(&sender, parent, &mut batch);
        }
        // Actual filesystem work is complete. Do not retain its permit behind
        // backpressure on the terminal event or another scan's permit request.
        drop(permit);
        let _ = sender.blocking_send(Event::DirectoryDone { parent, result });
    })
    .await
    .map_err(worker_error)?;
    Ok(())
}

fn resolve_entry(entry: ScanEntry, platform: &dyn Platform) -> Item {
    if entry.kind_hint == Some(NodeKind::Directory)
        || entry.info.as_ref().is_some_and(|info| {
            info.as_ref()
                .is_ok_and(|info| info.kind == NodeKind::Directory)
        })
    {
        return Item::Directory(entry.path);
    }
    let info = entry
        .info
        .unwrap_or_else(|| platform.scan_metadata(&entry.path));
    if info
        .as_ref()
        .is_ok_and(|info| info.kind == NodeKind::Directory)
    {
        Item::Directory(entry.path)
    } else {
        Item::Entry(entry.path, info)
    }
}
fn item_bytes(item: &Item) -> usize {
    let path_bytes = match item {
        Item::Entry(path, _) | Item::Directory(path) => path.as_os_str().as_encoded_bytes().len(),
        Item::Error(_) => 0,
    };
    let metadata_bytes = match item {
        Item::Entry(_, Ok(info)) => info.identity.len().saturating_add(info.volume_id.len()),
        Item::Entry(_, Err(error)) | Item::Error(error) => error.to_string().len(),
        Item::Directory(_) => 0,
    };
    size_of::<Item>()
        .saturating_add(path_bytes)
        .saturating_add(metadata_bytes)
}
fn send_batch(sender: &mpsc::Sender<Event>, parent: usize, batch: &mut Vec<Item>) -> bool {
    if batch.is_empty() {
        return true;
    }
    sender
        .blocking_send(Event::Batch {
            parent,
            items: std::mem::take(batch),
        })
        .is_ok()
}

async fn acquire_budget(
    budget: Arc<Semaphore>,
    cancelled: &AtomicBool,
    stop: &AtomicBool,
) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, AppError> {
    let acquire = budget.acquire_owned();
    tokio::pin!(acquire);
    loop {
        if stopped(cancelled, stop) {
            return Ok(None);
        }
        tokio::select! {
            permit = &mut acquire => return permit.map(Some).map_err(|error| AppError::new("worker_unavailable", error.to_string())),
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn closing_full_transport_releases_blocking_sender() {
        let (sender, receiver) = mpsc::channel(1);
        sender
            .send(Event::Batch {
                parent: 0,
                items: Vec::new(),
            })
            .await
            .unwrap();
        let blocked = tokio::task::spawn_blocking(move || {
            send_batch(
                &sender,
                0,
                &mut vec![Item::Directory(PathBuf::from("queued"))],
            )
        });
        drop(receiver);
        assert!(
            !tokio::time::timeout(Duration::from_secs(2), blocked)
                .await
                .unwrap()
                .unwrap()
        );
    }
    struct BackpressurePlatform {
        emitted: mpsc::Sender<usize>,
        finished: AtomicBool,
    }
    impl Platform for BackpressurePlatform {
        fn metadata(&self, _: &std::path::Path) -> io::Result<FileInfo> {
            Ok(FileInfo {
                private_bytes: None,
                kind: NodeKind::Directory,
                logical_bytes: 0,
                allocated_bytes: Some(0),
                modified_ns: None,
                identity: "directory".into(),
                volume_id: "volume".into(),
                link_count: 1,
                is_link_or_reparse: false,
            })
        }
        fn read_directory(
            &self,
            path: &std::path::Path,
            visitor: &mut dyn FnMut(DirectoryEvent) -> bool,
        ) -> io::Result<()> {
            for i in 0..3 {
                self.emitted.blocking_send(i).unwrap();
                let mut metadata = self.metadata(path)?;
                metadata.kind = NodeKind::File;
                if !visitor(DirectoryEvent::Entry(ScanEntry {
                    path: path.join(i.to_string()),
                    info: Some(Ok(metadata)),
                    kind_hint: Some(NodeKind::File),
                })) {
                    break;
                }
            }
            self.finished.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn volumes(&self) -> io::Result<Vec<crate::Volume>> {
            Ok(Vec::new())
        }
        fn trash(&self, _: &std::path::Path) -> io::Result<()> {
            unreachable!()
        }
        fn reveal(&self, _: &std::path::Path) -> io::Result<()> {
            unreachable!()
        }
        fn protected_paths(&self) -> Vec<PathBuf> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn cancellation_releases_a_full_directory_channel_and_its_only_permit() {
        let (emitted, mut attempts) = mpsc::channel(3);
        let platform = Arc::new(BackpressurePlatform {
            emitted,
            finished: AtomicBool::new(false),
        });
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = mpsc::channel(1);
        let budget = Arc::new(Semaphore::new(1));
        let worker = tokio::spawn(run_directory(
            Work {
                path: PathBuf::from("/virtual"),
                parent: None,
            },
            platform.clone(),
            cancelled.clone(),
            Arc::new(AtomicBool::new(false)),
            ScanOptions {
                workers: 1,
                batch_entries: 1,
                batch_bytes: 1024,
            },
            budget.clone(),
            sender,
        ));
        let Event::Admission { reply, .. } = receiver.recv().await.unwrap() else {
            panic!("expected admission");
        };
        reply.send(Some(0)).unwrap();
        assert_eq!(attempts.recv().await, Some(0));
        assert_eq!(attempts.recv().await, Some(1));
        // First callback completed its send; the second cannot fit until this
        // receiver drains, regardless of which thread is currently scheduled.
        assert_eq!(receiver.len(), 1);
        assert_eq!(budget.available_permits(), 0);
        cancelled.store(true, Ordering::SeqCst);
        receiver.close();
        while receiver.try_recv().is_ok() {}
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(platform.finished.load(Ordering::SeqCst));
        assert_eq!(budget.available_permits(), 1);
    }

    #[tokio::test]
    async fn closing_admission_reply_releases_the_blocking_worker_without_reading() {
        let (emitted, _attempts) = mpsc::channel(3);
        let platform = Arc::new(BackpressurePlatform {
            emitted,
            finished: AtomicBool::new(false),
        });
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = mpsc::channel(1);
        let budget = Arc::new(Semaphore::new(1));
        let worker = tokio::spawn(run_directory(
            Work {
                path: PathBuf::from("/virtual"),
                parent: None,
            },
            platform.clone(),
            cancelled.clone(),
            Arc::new(AtomicBool::new(false)),
            ScanOptions::default(),
            budget.clone(),
            sender,
        ));
        let Event::Admission { reply, .. } = receiver.recv().await.unwrap() else {
            panic!("expected admission");
        };
        assert_eq!(budget.available_permits(), 0);
        assert!(!platform.finished.load(Ordering::SeqCst));
        cancelled.store(true, Ordering::SeqCst);
        receiver.close();
        drop(reply);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(budget.available_permits(), 1);
        assert!(!platform.finished.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancellation_removes_a_worker_waiting_for_the_filesystem_budget() {
        let (emitted, _) = mpsc::channel(3);
        let platform = Arc::new(BackpressurePlatform {
            emitted,
            finished: AtomicBool::new(false),
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, _receiver) = mpsc::channel(1);
        let worker = tokio::spawn(run_directory(
            Work {
                path: PathBuf::from("/virtual"),
                parent: None,
            },
            platform.clone(),
            cancel.clone(),
            Arc::new(AtomicBool::new(false)),
            ScanOptions::default(),
            Arc::new(Semaphore::new(0)),
            sender,
        ));
        tokio::task::yield_now().await;
        cancel.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!platform.finished.load(Ordering::SeqCst));
    }
}

fn apply_event(
    event: Event,
    builder: &mut ScanBuilder,
    ready: &mut Vec<Work>,
    children: &mut HashMap<usize, Vec<Work>>,
    cancelled: &AtomicBool,
    stop: &AtomicBool,
) {
    match event {
        Event::Admission { work, info, reply } => {
            let admitted = if stopped(cancelled, stop) {
                None
            } else {
                builder.add(&work.path, work.parent, info)
            };
            let _ = reply.send(admitted);
        }
        Event::Batch { parent, items } => {
            for item in items {
                if stopped(cancelled, stop) {
                    break;
                }
                match item {
                    Item::Entry(path, info) => {
                        builder.add(&path, Some(parent), info);
                    }
                    Item::Directory(path) => children.entry(parent).or_default().push(Work {
                        path,
                        parent: Some(parent),
                    }),
                    Item::Error(error) => builder.directory_error(parent, &error),
                }
            }
        }
        Event::DirectoryDone { parent, result } => {
            if let Err(error) = result {
                builder.directory_error(parent, &error);
            }
            // Children cannot be checked before their parent's
            // enumeration returns. Fresh metadata is taken by
            // their own worker immediately before admission.
            if let Some(mut pending) = children.remove(&parent) {
                ready.append(&mut pending);
            }
        }
    }
}

async fn prepare_async(
    root: PathBuf,
    platform: Arc<dyn Platform>,
    budget: Arc<Semaphore>,
    cancelled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> Result<Option<(PathBuf, String, Option<crate::SpaceObservation>)>, AppError> {
    if !root.is_absolute() {
        return Err(AppError::new(
            "path_not_allowed",
            "Scan root must be absolute",
        ));
    }
    let Some(permit) = acquire_budget(budget, &cancelled, &stop).await? else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if stopped(&cancelled, &stop) {
            return Ok(None);
        }
        prepare_root(&root, platform.as_ref()).map(Some)
    })
    .await
    .map_err(worker_error)?
}

async fn observe_after(
    root: PathBuf,
    platform: Arc<dyn Platform>,
    volume: String,
    budget: Arc<Semaphore>,
    cancelled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> Result<Option<crate::SpaceObservation>, AppError> {
    let Some(permit) = acquire_budget(budget, &cancelled, &stop).await? else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if stopped(&cancelled, &stop) {
            None
        } else {
            matching_observation(platform.as_ref(), &root, &volume)
        }
    })
    .await
    .map_err(worker_error)
}

fn cancelled_snapshot(
    root: &std::path::Path,
    cancelled: &AtomicBool,
    progress: &watch::Sender<ScanProgress>,
) -> Snapshot {
    // No filesystem work started. Retain the requested native absolute root
    // instead of canonicalizing or inventing a volume observation.
    let (snapshot, counts) = ScanBuilder::new(root, String::new(), None).finish(cancelled, None);
    progress.send_replace(counts);
    snapshot
}
