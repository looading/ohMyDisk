use ohmydisk_core::*;
use ohmydisk_platform::NativePlatform;
use ohmydisk_service::{Engine, store::Store};
use serde_json::{Value, json};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct FixturePlatform {
    recycle: PathBuf,
    trash_calls: AtomicUsize,
}
impl Platform for FixturePlatform {
    fn metadata(&self, p: &Path) -> io::Result<FileInfo> {
        NativePlatform.metadata(p)
    }
    fn volumes(&self) -> io::Result<Vec<Volume>> {
        Ok(vec![])
    }
    fn trash(&self, p: &Path) -> io::Result<()> {
        self.trash_calls.fetch_add(1, Ordering::SeqCst);
        std::fs::rename(p, self.recycle.join(new_id()))
    }
    fn reveal(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "No graphical session in test",
        ))
    }
    fn protected_paths(&self) -> Vec<PathBuf> {
        vec![]
    }
}
async fn wait(engine: &Arc<Engine>, job: &Value) -> Job {
    let id = job["job_id"].as_str().unwrap();
    for _ in 0..1000 {
        let value = engine
            .handle("get_job", json!({"job_id":id}))
            .await
            .unwrap();
        let job: Job = serde_json::from_value(value).unwrap();
        if !matches!(
            job.state.as_str(),
            "queued" | "running" | "cancel_requested"
        ) {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("job timed out: {id}")
}
#[tokio::test]
async fn durable_idempotency_mode_isolation_and_refresh() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("files");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("keep.txt"), b"keep").unwrap();
    std::fs::write(root.join("remove.txt"), b"remove").unwrap();
    let recycle = fixture.path().join("recycle");
    std::fs::create_dir(&recycle).unwrap();
    let platform = Arc::new(FixturePlatform {
        recycle,
        trash_calls: AtomicUsize::new(0),
    });
    let state = fixture.path().join("state");
    let engine = Engine::open(state.clone(), platform.clone()).unwrap();
    let input = json!({"root_path":root,"request_key":"scan-one"});
    let scan = engine.handle("start_scan", input.clone()).await.unwrap();
    assert_eq!(engine.handle("start_scan", input).await.unwrap(), scan);
    assert_eq!(
        engine
            .handle(
                "start_scan",
                json!({"root_path":fixture.path(),"request_key":"scan-one"})
            )
            .await
            .unwrap_err()
            .code,
        "request_conflict"
    );
    assert_eq!(wait(&engine, &scan).await.state, "completed");
    let overlapping = engine
        .handle(
            "start_scan",
            json!({"root_path":root,"request_key":"scan-overlap"}),
        )
        .await
        .unwrap();
    assert_eq!(wait(&engine, &overlapping).await.state, "completed");
    let scan_id = scan["scan_id"].as_str().unwrap();
    let summary = engine
        .handle("get_scan", json!({"scan_id":scan_id}))
        .await
        .unwrap();
    assert!(summary.get("nodes").is_none());
    let nodes = engine
        .handle(
            "list_children",
            json!({"scan_id":scan_id,"node_id":summary["root_id"]}),
        )
        .await
        .unwrap();
    let remove = nodes["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "remove.txt")
        .unwrap();
    let prepared=engine.handle("create_cleanup_plan",json!({"scan_id":scan_id,"node_ids":[remove["id"]],"mode":"trash","request_key":"plan-one"})).await.unwrap();
    assert_eq!(wait(&engine, &prepared).await.state, "completed");
    let plan = engine
        .handle("get_cleanup_plan", json!({"plan_id":prepared["plan_id"]}))
        .await
        .unwrap();
    assert_eq!(
        engine
            .handle("release_scan", json!({"scan_id":scan_id}))
            .await
            .unwrap_err()
            .code,
        "plan_in_use"
    );
    let execute =
        json!({"plan_id":plan["id"],"plan_hash":plan["hash"],"request_key":"execute-one"});
    assert_eq!(
        engine
            .handle("execute_permanent", execute.clone())
            .await
            .unwrap_err()
            .code,
        "mode_mismatch"
    );
    assert!(root.join("remove.txt").exists());
    let cleaning = engine
        .handle("execute_trash", execute.clone())
        .await
        .unwrap();
    assert_eq!(
        engine
            .handle("execute_trash", execute.clone())
            .await
            .unwrap(),
        cleaning
    );
    let mut second = execute;
    second["request_key"] = json!("another-key");
    assert_eq!(
        engine.handle("execute_trash", second).await.unwrap(),
        cleaning
    );
    assert_eq!(wait(&engine, &cleaning).await.state, "completed");
    assert!(!root.join("remove.txt").exists());
    assert!(root.join("keep.txt").exists());
    assert_eq!(platform.trash_calls.load(Ordering::SeqCst), 1);
    let result = engine
        .handle("get_cleanup_result", cleaning.clone())
        .await
        .unwrap();
    assert!(
        result["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["status"] == "succeeded")
    );
    let refreshed = wait(&engine, &json!({"job_id":result["refresh_job_id"]})).await;
    assert_eq!(refreshed.state, "completed");
    assert_ne!(refreshed.result["snapshot_id"], summary["snapshot_id"]);
    let overlap_summary = engine
        .handle("get_scan", json!({"scan_id":overlapping["scan_id"]}))
        .await
        .unwrap();
    assert_eq!(overlap_summary["known_stale"], true);
    assert_eq!(overlap_summary["needs_rescan"], true);
    assert!(overlap_summary["refresh_job_id"].is_null());
    let refresh_summary = engine
        .handle("get_scan", json!({"scan_id":refreshed.result["scan_id"]}))
        .await
        .unwrap();
    assert_eq!(refresh_summary["known_stale"], false);
    assert_eq!(refresh_summary["needs_rescan"], false);
    assert!(
        engine
            .handle("get_scan", json!({"scan_id":scan_id}))
            .await
            .unwrap()["known_stale"]
            .as_bool()
            .unwrap()
    );
    for _ in 0..100 {
        if !engine.has_active_jobs() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    drop(engine);
    let engine = Engine::open(state, platform.clone()).unwrap();
    let mut replay =
        json!({"plan_id":plan["id"],"plan_hash":plan["hash"],"request_key":"after-restart"});
    assert_eq!(
        engine
            .handle("execute_trash", replay.clone())
            .await
            .unwrap(),
        cleaning
    );
    replay["plan_hash"] = json!("wrong");
    assert_eq!(
        engine
            .handle("execute_trash", replay)
            .await
            .unwrap_err()
            .code,
        "request_conflict"
    );
    assert_eq!(platform.trash_calls.load(Ordering::SeqCst), 1);
}
#[test]
fn crash_recovery_keeps_unknown_operations_and_requests() {
    let fixture = tempfile::tempdir().unwrap();
    let db = fixture.path().join("state.sqlite");
    let job = Job {
        id: "job".into(),
        kind: "cleanup".into(),
        state: "running".into(),
        created_at: now_ms(),
        progress: ScanProgress::default(),
        result: json!({"plan_id":"plan"}),
        error: None,
    };
    {
        let store = Store::open(&db).unwrap();
        assert!(
            store
                .accept_job(
                    "execute_trash",
                    "key",
                    "digest",
                    json!({"job_id":"job"}),
                    &job,
                    &[],
                    Some("plan")
                )
                .unwrap()
                .1
        );
        store
            .record_item(
                "job",
                &ItemResult {
                    node_id: "node".into(),
                    path: "temporary fixture".into(),
                    status: ItemStatus::Started,
                    code: None,
                    message: None,
                },
            )
            .unwrap();
    }
    let store = Store::open(&db).unwrap();
    assert_eq!(
        store.get::<Job>("job", "job").unwrap().unwrap().state,
        "interrupted"
    );
    assert_eq!(store.items("job").unwrap()[0].status, ItemStatus::InDoubt);
    assert_eq!(store.execution("plan").unwrap(), Some("job".into()));
    assert_eq!(
        store.request("execute_trash", "key", "digest").unwrap(),
        Some(json!({"job_id":"job"}))
    );
    assert_eq!(
        store
            .request("execute_trash", "key", "different")
            .unwrap_err()
            .code,
        "request_conflict"
    );
}

#[tokio::test]
async fn state_directory_descendants_cannot_be_authorized() {
    let fixture = tempfile::tempdir().unwrap();
    let recycle = fixture.path().join("recycle");
    std::fs::create_dir(&recycle).unwrap();
    let platform = Arc::new(FixturePlatform {
        recycle,
        trash_calls: AtomicUsize::new(0),
    });
    let state = fixture.path().join("state");
    let engine = Engine::open(state.clone(), platform).unwrap();
    let scan = engine
        .handle(
            "start_scan",
            json!({"root_path":fixture.path(),"request_key":"state-scan"}),
        )
        .await
        .unwrap();
    assert_eq!(wait(&engine, &scan).await.state, "completed");
    let nodes=engine.handle("query_nodes",json!({"scan_id":scan["scan_id"],"descendants":true,"name":"state.sqlite","kind":"file"})).await.unwrap();
    let target = nodes["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "state.sqlite")
        .unwrap();
    let plan=engine.handle("create_cleanup_plan",json!({"scan_id":scan["scan_id"],"node_ids":[target["id"]],"mode":"permanent","request_key":"state-plan"})).await.unwrap();
    let job = wait(&engine, &plan).await;
    assert_eq!(job.state, "failed");
    assert_eq!(job.error.unwrap().code, "path_not_allowed");
    assert!(state.join("state.sqlite").is_file());
}

/// Hold the initial metadata call so cancellation deterministically arrives before
/// traversal; no timing assumptions about the size or speed of the filesystem.
struct PausedPlatform {
    entered: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl Platform for PausedPlatform {
    fn metadata(&self, path: &Path) -> io::Result<FileInfo> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            entered.send(()).unwrap();
            self.resume
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        NativePlatform.metadata(path)
    }
    fn volumes(&self) -> io::Result<Vec<Volume>> {
        Ok(vec![])
    }
    fn trash(&self, _: &Path) -> io::Result<()> {
        panic!("Cancellation must not perform cleanup")
    }
    fn reveal(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn protected_paths(&self) -> Vec<PathBuf> {
        vec![]
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_is_requested_before_a_worker_reaches_its_next_boundary() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("files");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("untouched.txt"), b"data").unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let platform = Arc::new(PausedPlatform {
        entered: std::sync::Mutex::new(Some(entered_tx)),
        resume: std::sync::Mutex::new(resume_rx),
    });
    let engine = Engine::open(fixture.path().join("state"), platform).unwrap();
    let scan = engine
        .handle(
            "start_scan",
            json!({"root_path":root,"request_key":"cancel-scan"}),
        )
        .await
        .unwrap();
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let requested = engine
        .handle("cancel_job", json!({"job_id":scan["job_id"]}))
        .await
        .unwrap();
    assert_eq!(requested["state"], "cancel_requested");
    resume_tx.send(()).unwrap();
    let job = wait(&engine, &scan).await;
    assert_eq!(job.state, "cancelled");
    let summary = engine
        .handle("get_scan", json!({"scan_id":scan["scan_id"]}))
        .await
        .unwrap();
    assert_eq!(summary["complete"], false);
    assert!(root.join("untouched.txt").exists());
}

async fn idle(engine: &Arc<Engine>) {
    for _ in 0..1000 {
        if !engine.has_active_jobs() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("Engine workers did not finish");
}
fn interrupted_fixture_job(id: &str, scan_id: &str) -> Job {
    Job {
        id: id.into(),
        kind: "cleanup".into(),
        state: "running".into(),
        created_at: now_ms(),
        progress: ScanProgress::default(),
        result: json!({"scan_id":scan_id,"plan_id":"simulated-plan","items_ready":false,"needs_rescan":false}),
        error: None,
    }
}
fn fixture_item(id: &str, status: ItemStatus) -> ItemResult {
    ItemResult {
        node_id: id.into(),
        path: format!("temporary fixture/{id}"),
        status,
        code: None,
        message: None,
    }
}

#[tokio::test]
async fn interrupted_cleanup_invalidates_captures_once_without_replaying_deletion() {
    for persisted_status in [ItemStatus::Started, ItemStatus::Succeeded] {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("files");
        std::fs::create_dir(&root).unwrap();
        let untouched = root.join("keep.txt");
        std::fs::write(&untouched, b"untouched by recovery").unwrap();
        let recycle = fixture.path().join("recycle");
        std::fs::create_dir(&recycle).unwrap();
        let platform = Arc::new(FixturePlatform {
            recycle,
            trash_calls: AtomicUsize::new(0),
        });
        let state = fixture.path().join("state");
        let engine = Engine::open(state.clone(), platform.clone()).unwrap();
        let scan = engine
            .handle(
                "start_scan",
                json!({"root_path":root,"request_key":"before-crash"}),
            )
            .await
            .unwrap();
        assert_eq!(wait(&engine, &scan).await.state, "completed");
        idle(&engine).await;
        drop(engine);
        // Simulate the persistent cut point after an OS call or immediately after
        // its write-ahead Started record, before staleness/refresh was recorded.
        {
            let store = Store::open(&state.join("state.sqlite")).unwrap();
            let job = interrupted_fixture_job("crashed-cleanup", scan["scan_id"].as_str().unwrap());
            store.put("job", &job.id, &job).unwrap();
            store
                .record_item(&job.id, &fixture_item("target", persisted_status))
                .unwrap();
        }
        let engine = Engine::open(state.clone(), platform.clone()).unwrap();
        let old = engine
            .handle("get_scan", json!({"scan_id":scan["scan_id"]}))
            .await
            .unwrap();
        assert_eq!(old["known_stale"], true);
        assert_eq!(old["needs_rescan"], true);
        assert!(old["refresh_job_id"].is_null());
        let outcome = engine
            .handle("get_cleanup_result", json!({"job_id":"crashed-cleanup"}))
            .await
            .unwrap();
        assert_eq!(outcome["state"], "interrupted");
        assert_eq!(outcome["needs_rescan"], true);
        assert!(outcome["refresh_job_id"].is_null());
        assert!(
            outcome["recovery_message"]
                .as_str()
                .unwrap()
                .contains("No deletion will be replayed")
        );
        let expected = if persisted_status == ItemStatus::Started {
            "in_doubt"
        } else {
            "succeeded"
        };
        assert_eq!(outcome["items"][0]["status"], expected);
        assert!(!engine.has_active_jobs());
        assert_eq!(platform.trash_calls.load(Ordering::SeqCst), 0);
        assert!(untouched.exists());
        // A capture explicitly obtained after recovery must stay fresh across the
        // next restart: the old interrupted job is not a perpetual invalidator.
        let fresh = engine
            .handle(
                "start_scan",
                json!({"root_path":root,"request_key":"after-crash"}),
            )
            .await
            .unwrap();
        assert_eq!(wait(&engine, &fresh).await.state, "completed");
        idle(&engine).await;
        drop(engine);
        let engine = Engine::open(state, platform).unwrap();
        let summary = engine
            .handle("get_scan", json!({"scan_id":fresh["scan_id"]}))
            .await
            .unwrap();
        assert_eq!(summary["known_stale"], false);
        assert_eq!(summary["needs_rescan"], false);
    }
}

#[tokio::test]
async fn cleanup_pagination_waits_for_fixed_membership_and_updates_cannot_add_nodes() {
    let fixture = tempfile::tempdir().unwrap();
    let recycle = fixture.path().join("recycle");
    std::fs::create_dir(&recycle).unwrap();
    let state = fixture.path().join("state");
    let platform = Arc::new(FixturePlatform {
        recycle,
        trash_calls: AtomicUsize::new(0),
    });
    let engine = Engine::open(state.clone(), platform).unwrap();
    // Inject partial initialization while this Engine is live. Opening it after
    // injection would correctly recover the fixture job as interrupted instead.
    let store = Store::open(&state.join("state.sqlite")).unwrap();
    let job = interrupted_fixture_job("initializing", "unused-scan");
    store.put("job", &job.id, &job).unwrap();
    for id in ["z", "a", "m"] {
        store
            .record_item(&job.id, &fixture_item(id, ItemStatus::NotStarted))
            .unwrap();
        let error = engine
            .handle("get_cleanup_result", json!({"job_id":job.id,"limit":1}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "result_not_ready");
    }
    store
        .update::<Job>("job", &job.id, |j| j.result["items_ready"] = json!(true))
        .unwrap();
    let first = engine
        .handle("get_cleanup_result", json!({"job_id":job.id,"limit":1}))
        .await
        .unwrap();
    assert_eq!(first["items"][0]["node_id"], "a");
    // Values may change while a cursor is in use; membership and ordering cannot.
    store
        .update_item(&job.id, &fixture_item("a", ItemStatus::Succeeded))
        .unwrap();
    assert_eq!(
        store
            .update_item(&job.id, &fixture_item("b", ItemStatus::Started))
            .unwrap_err()
            .code,
        "state_error"
    );
    let second = engine
        .handle(
            "get_cleanup_result",
            json!({"job_id":job.id,"limit":1,"cursor":first["next_cursor"]}),
        )
        .await
        .unwrap();
    let third = engine
        .handle(
            "get_cleanup_result",
            json!({"job_id":job.id,"limit":1,"cursor":second["next_cursor"]}),
        )
        .await
        .unwrap();
    assert_eq!(second["items"][0]["node_id"], "m");
    assert_eq!(third["items"][0]["node_id"], "z");
    assert!(third["next_cursor"].is_null());
    assert_eq!(store.items(&job.id).unwrap().len(), 3);
    // Initialization that terminates early also has a fixed collection and must
    // remain inspectable, even though the complete plan was never seeded.
    let mut failed = interrupted_fixture_job("init-failed", "unused-scan");
    failed.state = "failed".into();
    store.put("job", &failed.id, &failed).unwrap();
    store
        .record_item(
            &failed.id,
            &fixture_item("only-recorded-item", ItemStatus::NotStarted),
        )
        .unwrap();
    let result = engine
        .handle("get_cleanup_result", json!({"job_id":failed.id}))
        .await
        .unwrap();
    assert_eq!(result["items_ready"], true);
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn cached_snapshot_pages_survive_restart_and_release_removes_blob() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("files");
    std::fs::create_dir(&root).unwrap();
    for n in 1..=7 {
        std::fs::write(root.join(format!("file-{n}")), vec![0u8; n]).unwrap();
    }
    let state = fixture.path().join("state");
    let engine = Engine::open(state.clone(), Arc::new(NativePlatform)).unwrap();
    let scan = engine
        .handle(
            "start_scan",
            json!({"root_path":root,"request_key":"cached-pages"}),
        )
        .await
        .unwrap();
    let job = wait(&engine, &scan).await;
    assert_eq!(job.state, "completed");
    assert_eq!(job.result["snapshot_codec"], "lz4-json-v1");
    let summary = engine
        .handle("get_scan", json!({"scan_id":scan["scan_id"]}))
        .await
        .unwrap();
    let query = json!({"scan_id":scan["scan_id"],"node_id":summary["root_id"],"limit":2});
    let first = engine.handle("list_children", query.clone()).await.unwrap();
    assert_eq!(
        first,
        engine.handle("list_children", query.clone()).await.unwrap()
    );
    let mut second_query = query;
    second_query["cursor"] = first["next_cursor"].clone();
    let second = engine
        .handle("list_children", second_query.clone())
        .await
        .unwrap();
    while engine.has_active_jobs() {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    drop(engine);
    let engine = Engine::open(state.clone(), Arc::new(NativePlatform)).unwrap();
    assert_eq!(
        second,
        engine.handle("list_children", second_query).await.unwrap()
    );
    engine
        .handle("release_scan", json!({"scan_id":scan["scan_id"]}))
        .await
        .unwrap();
    assert_eq!(
        engine
            .handle("query_nodes", json!({"scan_id":scan["scan_id"]}))
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    let db = rusqlite::Connection::open(state.join("state.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM snapshot_blobs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn legacy_snapshot_query_is_compatible_without_rewriting_legacy_data() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("files");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("legacy"), b"legacy data").unwrap();
    let mut snapshot =
        ohmydisk_core::scan::scan(&root, &NativePlatform, &false.into(), &mut |_| {}).unwrap();
    let state = fixture.path().join("state");
    ohmydisk_platform::prepare_private_dir(&state).unwrap();
    snapshot.space_accounting = None;
    for node in &mut snapshot.nodes {
        node.space_usage = None;
    }
    let store = Store::open(&state.join("state.sqlite")).unwrap();
    let job_id = new_id();
    store.put("snapshot", &snapshot.id, &snapshot).unwrap();
    store.put("scan",&snapshot.scan_id,&json!({"scan_id":snapshot.scan_id,"job_id":job_id,"root_path":snapshot.root_path,"created_at":snapshot.created_at,"snapshot_id":snapshot.id,"root_id":snapshot.root_id,"logical_bytes":snapshot.nodes[0].logical_bytes,"complete":snapshot.complete,"issue_count":snapshot.issues.len(),"known_stale":false,"refresh_job_id":null})).unwrap();
    store
        .put(
            "job",
            &job_id,
            &Job {
                id: job_id.clone(),
                kind: "scan".into(),
                state: "completed".into(),
                created_at: 0,
                progress: ScanProgress::default(),
                result: json!({"scan_id":snapshot.scan_id}),
                error: None,
            },
        )
        .unwrap();
    drop(store);
    let engine = Engine::open(state.clone(), Arc::new(NativePlatform)).unwrap();
    let mut expected = ohmydisk_core::query::query(
        &snapshot,
        &Query {
            descendants: true,
            ..Default::default()
        },
    )
    .unwrap();
    expected["known_stale"] = json!(false);
    assert_eq!(
        engine
            .handle(
                "query_nodes",
                json!({"scan_id":snapshot.scan_id,"descendants":true})
            )
            .await
            .unwrap(),
        expected
    );
    let db = rusqlite::Connection::open(state.join("state.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM snapshot_blobs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM entities WHERE kind='snapshot'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
