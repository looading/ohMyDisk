use crate::{
    api,
    snapshot_cache::{SnapshotCache, SnapshotView},
    store::Store,
};
use ohmydisk_core::{cleanup, scan, *};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
struct ScanRecord {
    scan_id: String,
    job_id: String,
    root_path: NativePath,
    created_at: u64,
    snapshot_id: Option<String>,
    root_id: Option<String>,
    logical_bytes: u64,
    complete: bool,
    issue_count: usize,
    known_stale: bool,
    #[serde(default)]
    needs_rescan: bool,
    refresh_job_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct PlanRecord {
    id: String,
    job_id: String,
    scan_id: String,
    plan: Option<CleanupPlan>,
}

pub struct Engine {
    store: Arc<Store>,
    platform: Arc<dyn Platform>,
    data_dir: PathBuf,
    mutations: Mutex<()>,
    cleanup: Mutex<()>,
    cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>,
    active: AtomicUsize,
    workers: Arc<tokio::sync::Semaphore>,
    snapshots: SnapshotCache,
}
impl Engine {
    pub fn open(data_dir: PathBuf, platform: Arc<dyn Platform>) -> Result<Arc<Self>, AppError> {
        ohmydisk_platform::prepare_private_dir(&data_dir)?;
        let data_dir = std::fs::canonicalize(data_dir)?;
        let store = Arc::new(Store::open(&data_dir.join("state.sqlite"))?);
        let engine = Arc::new(Self {
            store,
            platform,
            data_dir,
            mutations: Mutex::new(()),
            cleanup: Mutex::new(()),
            cancellations: Mutex::new(HashMap::new()),
            active: AtomicUsize::new(0),
            workers: Arc::new(tokio::sync::Semaphore::new(2)),
            snapshots: SnapshotCache::default(),
        });
        engine.recover_scan_staleness()?;
        Ok(engine)
    }
    pub fn has_active_jobs(&self) -> bool {
        self.active.load(Ordering::SeqCst) > 0
    }
    pub async fn handle(self: &Arc<Self>, operation: &str, args: Value) -> Result<Value, AppError> {
        let op = operation.strip_prefix("omd_").unwrap_or(operation);
        if !args.is_object() {
            return Err(AppError::new(
                "invalid_arguments",
                "Arguments must be an object",
            ));
        }
        match op {
            "get_capabilities" => Ok(
                json!({"api_version":1,"platform":std::env::consts::OS,"logical_bytes":true,"allocated_bytes":cfg!(any(unix,windows)),"cleanup_modes":["trash","permanent"],"authorization_model":"host_confirmed","scope_policy":{"absolute_local_roots":true,"follow_links":false,"cross_filesystems":false,"protected_state_directory":NativePath::new(&self.data_dir)},"snapshots":"immutable","space_reclaimed":"observe_volume_available_bytes; trash does not promise freed space"}),
            ),
            "list_sources" => Ok(json!({"items":self.platform.volumes()?})),
            "start_scan" => self.start_scan(args),
            "list_scans" => {
                let mut records = self.store.all::<ScanRecord>("scan")?;
                records.sort_by(|a, b| {
                    b.created_at
                        .cmp(&a.created_at)
                        .then(a.scan_id.cmp(&b.scan_id))
                });
                let summaries = records
                    .iter()
                    .map(|r| self.scan_summary(r))
                    .collect::<Result<Vec<_>, _>>()?;
                let binding = api::digest(&json!(
                    records.iter().map(|r| &r.scan_id).collect::<Vec<_>>()
                ));
                api::page(&summaries, &args, &format!("scans:{binding}"))
            }
            "get_scan" => self.scan_summary(&self.scan_record(api::text(&args, "scan_id")?)?),
            "list_scan_issues" | "list_children" | "query_nodes" | "reveal_node" => {
                let engine = self.clone();
                let operation = op.to_owned();
                tokio::task::spawn_blocking(move || engine.snapshot_request(&operation, args))
                    .await
                    .map_err(|error| AppError::new("worker_panic", error.to_string()))?
            }
            "get_job" => {
                serde_json::to_value(self.job(api::text(&args, "job_id")?)?).map_err(serialization)
            }
            "cancel_job" => {
                let id = api::text(&args, "job_id")?;
                let mut job = self.job(id)?;
                if active_state(&job.state) {
                    if let Some(cancel) = self.cancellations.lock().map_err(poison)?.get(id) {
                        cancel.store(true, Ordering::SeqCst);
                    }
                    job = self.store.update::<Job>("job", id, |j| {
                        if active_state(&j.state) {
                            j.state = "cancel_requested".into();
                        }
                    })?;
                }
                Ok(json!(job))
            }
            "create_cleanup_plan" => self.create_plan(args),
            "get_cleanup_plan" => self.get_plan(args),
            "execute_trash" => self.execute(args, CleanupMode::Trash),
            "execute_permanent" => self.execute(args, CleanupMode::Permanent),
            "get_cleanup_result" => self.cleanup_result(args),
            "release_scan" => self.release_scan(args),
            _ => Err(AppError::new(
                "unsupported",
                format!("Unknown operation: {op}"),
            )),
        }
    }
    fn job(&self, id: &str) -> Result<Job, AppError> {
        self.store.get("job", id)?.ok_or_else(|| missing("job"))
    }
    fn scan_record(&self, id: &str) -> Result<ScanRecord, AppError> {
        self.store.get("scan", id)?.ok_or_else(|| missing("scan"))
    }
    fn plan_record(&self, id: &str) -> Result<PlanRecord, AppError> {
        self.store.get("plan", id)?.ok_or_else(|| missing("plan"))
    }
    fn snapshot_request(&self, operation: &str, args: Value) -> Result<Value, AppError> {
        let snapshot = self.snapshot(api::text(&args, "scan_id")?)?;
        match operation {
            "list_scan_issues" => {
                let mut value =
                    api::page(&snapshot.issues, &args, &format!("issues:{}", snapshot.id))?;
                value["scan_id"] = json!(snapshot.scan_id);
                value["snapshot_id"] = json!(snapshot.id);
                Ok(value)
            }
            "reveal_node" => {
                let id = api::text(&args, "node_id")?;
                let node = snapshot
                    .nodes
                    .iter()
                    .find(|node| node.id == id)
                    .ok_or_else(|| missing("node"))?;
                self.platform.reveal(&node.path.to_path()?)?;
                Ok(json!({"revealed":true,"node_id":id}))
            }
            _ => {
                let mut filter: Query =
                    serde_json::from_value(args.clone()).map_err(serialization)?;
                if operation == "list_children" {
                    filter.scope_node_id = Some(api::text(&args, "node_id")?.into());
                    filter.descendants = false;
                }
                let mut value = snapshot.query(&filter)?;
                value["known_stale"] = json!(self.scan_record(&snapshot.scan_id)?.known_stale);
                Ok(value)
            }
        }
    }
    fn snapshot_by_id(&self, id: &str) -> Result<Arc<SnapshotView>, AppError> {
        self.snapshots.get_or_load(id, || {
            self.store
                .get_snapshot(id)?
                .ok_or_else(|| missing("snapshot"))
        })
    }
    fn snapshot(&self, id: &str) -> Result<Arc<SnapshotView>, AppError> {
        let record = self.scan_record(id)?;
        let snapshot_id = record.snapshot_id.ok_or_else(|| {
            AppError::new(
                "scan_not_ready",
                format!("Scan is not ready; job_id={}", record.job_id),
            )
        })?;
        self.snapshot_by_id(&snapshot_id)
    }
    fn scan_summary(&self, record: &ScanRecord) -> Result<Value, AppError> {
        let job = self.job(&record.job_id)?;
        Ok(
            json!({"scan_id":record.scan_id,"snapshot_id":record.snapshot_id,"root_id":record.root_id,"root_path":record.root_path,"logical_bytes":record.logical_bytes,"complete":record.complete,"created_at":record.created_at,"known_stale":record.known_stale,"needs_rescan":record.needs_rescan,"refresh_job_id":record.refresh_job_id,"job_id":record.job_id,"state":job.state,"issue_count":record.issue_count,"size_metric":"logical_bytes","captured_at":record.created_at}),
        )
    }
    fn start_scan(self: &Arc<Self>, args: Value) -> Result<Value, AppError> {
        let _guard = self.mutations.lock().map_err(poison)?;
        let (key, digest) = api::request(&args)?;
        if let Some(old) = self.store.request("start_scan", key, &digest)? {
            return Ok(old);
        }
        let path = path_argument(&args, "root_path")?;
        if !path.is_absolute() {
            return Err(AppError::new(
                "path_not_allowed",
                "Scan root must be an absolute local path",
            ));
        }
        let scan_id = new_id();
        let job_id = new_id();
        let record = ScanRecord {
            scan_id: scan_id.clone(),
            job_id: job_id.clone(),
            root_path: NativePath::new(&path),
            created_at: now_ms(),
            snapshot_id: None,
            root_id: None,
            logical_bytes: 0,
            complete: false,
            issue_count: 0,
            known_stale: false,
            needs_rescan: false,
            refresh_job_id: None,
        };
        let job = new_job(&job_id, "scan", json!({"scan_id":scan_id}));
        let response = json!({"scan_id":scan_id,"job_id":job_id});
        let (response, accepted) = self.store.accept_job(
            "start_scan",
            key,
            &digest,
            response,
            &job,
            &[("scan".into(), scan_id.clone(), json!(record))],
            None,
        )?;
        if accepted {
            self.launch_scan(job_id, scan_id, path);
        }
        Ok(response)
    }
    fn launch_scan(self: &Arc<Self>, job_id: String, scan_id: String, path: PathBuf) {
        self.launch(job_id, move |engine, job_id, cancel| {
            let started=Instant::now();
            let mut last_update=None::<Instant>;
            let mut progress_writes=0u64;
            let mut progress_write_ms=0.0;
            let mut snapshot=scan::scan(&path,engine.platform.as_ref(),&cancel,&mut |mut progress| {
                let now=Instant::now();
                // UI progress is noncritical; terminal states and cleanup
                // started/outcome records retain their existing FULL durability.
                if progress.phase=="scanning" && last_update.is_some_and(|last|now.duration_since(last)<Duration::from_millis(250)) {return;}
                if progress.phase=="completed" {progress.phase="saving".into();}
                let write=Instant::now();
                if engine.store.update::<Job>("job",&job_id,|j|j.progress=progress).is_ok() {
                    progress_writes+=1;last_update=Some(now);
                }
                progress_write_ms+=write.elapsed().as_secs_f64()*1000.0;
            })?;
            let scan_ms=started.elapsed().as_secs_f64()*1000.0;
            snapshot.scan_id=scan_id.clone();
            let state=if snapshot.cancelled {"cancelled"}else if snapshot.complete {"completed"}else{"completed_with_errors"};
            let persist=Instant::now();
            let storage=engine.store.put_snapshot(&snapshot)?;
            let persist_ms=persist.elapsed().as_secs_f64()*1000.0;
            engine.store.update::<Job>("job",&job_id,|j|j.progress.phase="indexing".into())?;
            let indexing=Instant::now();
            let snapshot=engine.snapshots.insert(snapshot)?;
            let index_build_ms=indexing.elapsed().as_secs_f64()*1000.0;
            engine.store.update::<ScanRecord>("scan",&scan_id,|r| {
                r.root_path=snapshot.root_path.clone();r.snapshot_id=Some(snapshot.id.clone());r.root_id=Some(snapshot.root_id.clone());r.complete=snapshot.complete;r.created_at=snapshot.created_at;r.issue_count=snapshot.issues.len();
                r.logical_bytes=snapshot.nodes.iter().find(|n|n.id==snapshot.root_id).map_or(0,|n|n.logical_bytes);
            })?;
            engine.store.update::<Job>("job",&job_id,|j|j.result=json!({
                "scan_id":scan_id,"snapshot_id":snapshot.id,"complete":snapshot.complete,"issue_count":snapshot.issues.len(),
                "scan_ms":scan_ms,"persist_ms":persist_ms,"progress_writes":progress_writes,"progress_write_ms":progress_write_ms,
                "snapshot_codec":storage.codec,"snapshot_stored_bytes":storage.stored_bytes,"snapshot_json_bytes":storage.json_bytes,
                "index_build_ms":index_build_ms,
            }))?;
            Ok(state.into())
        });
    }
    fn create_plan(self: &Arc<Self>, mut args: Value) -> Result<Value, AppError> {
        // Normalize target set before request-key comparison: selection order is immaterial.
        let mut ids: Vec<String> =
            serde_json::from_value(args.get("node_ids").cloned().unwrap_or(Value::Null))
                .map_err(serialization)?;
        ids.sort();
        ids.dedup();
        args["node_ids"] = json!(ids);
        let mode: CleanupMode =
            serde_json::from_value(args.get("mode").cloned().unwrap_or(json!("trash")))
                .map_err(serialization)?;
        args["mode"] = json!(mode);
        let _guard = self.mutations.lock().map_err(poison)?;
        let (key, digest) = api::request(&args)?;
        if let Some(old) = self.store.request("create_cleanup_plan", key, &digest)? {
            return Ok(old);
        }
        let scan_id = api::text(&args, "scan_id")?.to_owned();
        let snapshot_id = self.scan_record(&scan_id)?.snapshot_id.ok_or_else(|| {
            AppError::new(
                "scan_not_ready",
                "Scan must finish before preparing a cleanup plan",
            )
        })?;
        let plan_id = new_id();
        let job_id = new_id();
        let record = PlanRecord {
            id: plan_id.clone(),
            job_id: job_id.clone(),
            scan_id: scan_id.clone(),
            plan: None,
        };
        let job = new_job(
            &job_id,
            "prepare_plan",
            json!({"plan_id":plan_id,"scan_id":scan_id}),
        );
        let response = json!({"plan_id":plan_id,"job_id":job_id});
        let (response, accepted) = self.store.accept_job(
            "create_cleanup_plan",
            key,
            &digest,
            response,
            &job,
            &[("plan".into(), plan_id.clone(), json!(record))],
            None,
        )?;
        if accepted {
            self.launch(job_id,move |engine,job_id,cancel|{
            if cancel.load(Ordering::SeqCst){return Ok("cancelled".into());}
            let snapshot=engine.snapshot_by_id(&snapshot_id)?;
            let mut protected=engine.platform.protected_paths();protected.push(engine.data_dir.clone());
            let mut plan=cleanup::prepare_plan(&snapshot,&ids,mode,&protected)?;plan.id=plan_id.clone();cleanup::rehash_plan(&mut plan)?;
            let mut subtrees=engine.platform.protected_subtrees();subtrees.push(engine.data_dir.clone());cleanup::validate_plan_scope(&plan,&subtrees,engine.platform.as_ref())?;
            engine.store.update::<PlanRecord>("plan",&plan_id,|r|r.plan=Some(plan.clone()))?;
            engine.store.update::<Job>("job",&job_id,|j|j.result=json!({"plan_id":plan.id,"plan_hash":plan.hash,"scan_id":plan.scan_id,"logical_bytes":plan.logical_bytes}))?;
            Ok("completed".into())
        });
        }
        Ok(response)
    }
    fn get_plan(&self, args: Value) -> Result<Value, AppError> {
        let record = self.plan_record(api::text(&args, "plan_id")?)?;
        let Some(plan) = record.plan else {
            return Ok(
                json!({"id":record.id,"job_id":record.job_id,"state":self.job(&record.job_id)?.state,"ready":false}),
            );
        };
        let mut seen = HashSet::new();
        let members = plan
            .targets
            .iter()
            .flat_map(|t| t.members.iter())
            .filter(|n| seen.insert(n.id.clone()))
            .collect::<Vec<_>>();
        let mut result = api::page(&members, &args, &format!("plan:{}:{}", plan.id, plan.hash))?;
        result["id"] = json!(plan.id);
        result["plan_id"] = json!(plan.id);
        result["hash"] = json!(plan.hash);
        result["plan_hash"] = json!(plan.hash);
        result["mode"] = json!(plan.mode);
        result["logical_bytes"] = json!(plan.logical_bytes);
        result["expires_at"] = json!(plan.expires_at);
        result["targets"] = json!(plan.targets.iter().map(|t| &t.node).collect::<Vec<_>>());
        result["scan_id"] = json!(plan.scan_id);
        result["snapshot_id"] = json!(plan.snapshot_id);
        result["job_id"] = json!(record.job_id);
        result["execution_job_id"] = json!(self.store.execution(&plan.id)?);
        result["ready"] = json!(true);
        result["member_count"] = json!(members.len());
        Ok(result)
    }
    fn execute(self: &Arc<Self>, args: Value, mode: CleanupMode) -> Result<Value, AppError> {
        let operation = if mode == CleanupMode::Trash {
            "execute_trash"
        } else {
            "execute_permanent"
        };
        let _guard = self.mutations.lock().map_err(poison)?;
        let (key, digest) = api::request(&args)?;
        if let Some(old) = self.store.request(operation, key, &digest)? {
            return Ok(old);
        }
        let record = self.plan_record(api::text(&args, "plan_id")?)?;
        let plan = record.plan.ok_or_else(|| {
            AppError::new(
                "plan_not_ready",
                format!("Plan is not ready; job_id={}", record.job_id),
            )
        })?;
        if plan.mode != mode {
            return Err(AppError::new(
                "mode_mismatch",
                "Plan mode does not match this execution tool",
            ));
        }
        if api::text(&args, "plan_hash")? != plan.hash {
            return Err(AppError::new(
                "stale_plan",
                "Plan hash does not match the immutable plan",
            ));
        }
        if self.store.execution(&plan.id)?.is_none() {
            if now_ms() > plan.expires_at {
                return Err(AppError::new(
                    "plan_expired",
                    "Plan has expired; prepare a new plan",
                ));
            }
            let mut subtrees = self.platform.protected_subtrees();
            subtrees.push(self.data_dir.clone());
            cleanup::validate_plan_scope(&plan, &subtrees, self.platform.as_ref())?;
        }
        let job_id = new_id();
        let job = new_job(
            &job_id,
            "cleanup",
            json!({"plan_id":plan.id,"scan_id":plan.scan_id,"mode":mode,"plan_hash":plan.hash,"authorization_model":"host_confirmed","space_before":null,"space_after":null,"refresh_job_id":null,"refresh_scan_id":null,"needs_rescan":false,"items_ready":false}),
        );
        let (response, accepted) = self.store.accept_job(
            operation,
            key,
            &digest,
            json!({"job_id":job_id}),
            &job,
            &[],
            Some(&plan.id),
        )?;
        if accepted {
            self.launch(job_id,move |engine,job_id,cancel|{
            // One service owns all cleanup; serialization also excludes overlapping plans.
            let _serial=engine.cleanup.lock().map_err(poison)?;
            let before=engine.space_observation();
            engine.store.update::<Job>("job",&job_id,|j|j.result["space_before"]=before)?;
            let mut seen=HashSet::new();
            let operation_nodes=if plan.mode==CleanupMode::Trash{plan.targets.iter().map(|t|&t.node).collect::<Vec<_>>()}else{plan.targets.iter().flat_map(|t|t.members.iter().chain(std::iter::once(&t.node))).collect::<Vec<_>>()};
            for node in operation_nodes.into_iter().filter(|n|seen.insert(n.id.clone())) {
                if cancel.load(Ordering::SeqCst) { return Ok("cancelled".into()); }
                engine.store.record_item(&job_id,&ItemResult{node_id:node.id.clone(),path:node.path.display.clone(),status:ItemStatus::NotStarted,code:None,message:None})?;
            }
            engine.store.update::<Job>("job",&job_id,|j|j.result["items_ready"]=json!(true))?;
            let outcome=cleanup::execute_plan(&plan,engine.platform.as_ref(),&cancel,&mut |item|engine.store.update_item(&job_id,&item));
            let after=engine.space_observation();
            engine.store.update::<Job>("job",&job_id,|j|j.result["space_after"]=after)?;
            // Started means a system operation may have run, including on error/cancellation.
            let mut items=engine.store.items(&job_id)?;
            for item in &mut items {if item.status==ItemStatus::Started {item.status=ItemStatus::InDoubt;item.code=Some("in_doubt".into());item.message=Some("The filesystem operation started but no final outcome could be recorded".into());engine.store.update_item(&job_id,item)?;}}
            let touched=items.iter().any(|i|matches!(i.status,ItemStatus::Succeeded|ItemStatus::Started|ItemStatus::InDoubt|ItemStatus::Failed|ItemStatus::Skipped));
            if touched {engine.refresh_after_cleanup(&plan,&job_id)?;}
            let results=outcome?;
            if cancel.load(Ordering::SeqCst){return Ok("cancelled".into());}
            Ok(if results.iter().any(|i|matches!(i.status,ItemStatus::Failed|ItemStatus::Skipped|ItemStatus::InDoubt)){"completed_with_errors"}else{"completed"}.into())
        });
        }
        Ok(response)
    }
    fn space_observation(&self) -> Value {
        match self.platform.volumes() {
            Ok(volumes) => json!(volumes),
            Err(error) => json!({"error":error.to_string()}),
        }
    }
    /// Recovery happens under the process-wide service lock, before new requests.
    /// The pending marker is cleared only after every old scan is invalidated, so
    /// a second crash repeats this safe metadata step rather than filesystem work.
    fn recover_scan_staleness(&self) -> Result<(), AppError> {
        let pending = self
            .store
            .all::<Job>("job")?
            .into_iter()
            .filter(|j| j.kind == "cleanup" && j.result["recovery_pending"] == true)
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }
        self.mark_existing_scans_stale()?;
        for job in pending {
            self.store.update::<Job>("job", &job.id, |j| {
                j.result["recovery_pending"] = json!(false);
                j.result["recovery_invalidated_scans"] = json!(true);
            })?;
        }
        Ok(())
    }
    /// Conservative invalidation includes overlapping scans and paths that may be
    /// aliases on a case-insensitive filesystem. Only the source root is rescanned
    /// automatically; other saved captures explicitly request a rescan.
    fn mark_existing_scans_stale(&self) -> Result<(), AppError> {
        let _guard = self.mutations.lock().map_err(poison)?;
        for record in self.store.all::<ScanRecord>("scan")? {
            self.store
                .update::<ScanRecord>("scan", &record.scan_id, |s| {
                    s.known_stale = true;
                    s.needs_rescan = true;
                    s.refresh_job_id = None;
                })?;
        }
        Ok(())
    }
    fn refresh_after_cleanup(
        self: &Arc<Self>,
        plan: &CleanupPlan,
        cleanup_job: &str,
    ) -> Result<(), AppError> {
        self.store.update::<Job>("job", cleanup_job, |j| {
            j.result["needs_rescan"] = json!(true)
        })?;
        self.mark_existing_scans_stale()?;
        let old = self.scan_record(&plan.scan_id)?;
        let response = self.start_scan(
            json!({"root_path":old.root_path,"request_key":format!("refresh:{cleanup_job}")}),
        )?;
        let refresh_job = response["job_id"].clone();
        let refresh_scan = response["scan_id"].clone();
        self.store
            .update::<ScanRecord>("scan", &plan.scan_id, |s| {
                s.known_stale = true;
                s.needs_rescan = false;
                s.refresh_job_id = refresh_job.as_str().map(str::to_owned);
            })?;
        self.store.update::<Job>("job", cleanup_job, |j| {
            j.result["refresh_job_id"] = refresh_job;
            j.result["refresh_scan_id"] = refresh_scan;
            j.result["needs_rescan"] = json!(false);
        })?;
        Ok(())
    }
    fn cleanup_result(&self, args: Value) -> Result<Value, AppError> {
        let id = api::text(&args, "job_id")?;
        let job = self.job(id)?;
        if job.kind != "cleanup" {
            return Err(AppError::new(
                "invalid_arguments",
                "job_id is not a cleanup job",
            ));
        }
        let readable = job.result["items_ready"] == true || !active_state(&job.state);
        if !readable {
            return Err(AppError::new(
                "result_not_ready",
                "Cleanup result membership is still being initialized; poll get_job before paginating results",
            ));
        }
        let items = self.store.items(id)?;
        let mut value = api::page(&items, &args, &format!("cleanup:{id}"))?;
        value["job_id"] = json!(id);
        value["state"] = json!(job.state);
        value["refresh_job_id"] = job.result["refresh_job_id"].clone();
        value["refresh_scan_id"] = job.result["refresh_scan_id"].clone();
        value["space_before"] = job.result["space_before"].clone();
        value["space_after"] = job.result["space_after"].clone();
        value["items_ready"] = json!(readable);
        value["items_initialization_complete"] = json!(job.result["items_ready"] == true);
        value["needs_rescan"] = json!(job.result["needs_rescan"] == true);
        value["recovery_message"] = job.result["recovery_message"].clone();
        value["error"] = json!(job.error);
        Ok(value)
    }
    fn release_scan(&self, args: Value) -> Result<Value, AppError> {
        let _guard = self.mutations.lock().map_err(poison)?;
        let id = api::text(&args, "scan_id")?;
        let record = self.scan_record(id)?;
        let mut references = Vec::new();
        if active_state(&self.job(&record.job_id)?.state) {
            references.push(json!({"job_id":record.job_id}));
        }
        for plan in self
            .store
            .all::<PlanRecord>("plan")?
            .into_iter()
            .filter(|p| p.scan_id == id)
        {
            let preparing = active_state(&self.job(&plan.job_id)?.state);
            let execution = self.store.execution(&plan.id)?;
            let executing = execution
                .as_ref()
                .map(|j| self.job(j).map(|j| active_state(&j.state)))
                .transpose()?
                .unwrap_or(false);
            let valid =
                plan.plan.as_ref().is_some_and(|p| p.expires_at >= now_ms()) && execution.is_none();
            if preparing || executing || valid {
                references
                    .push(json!({"plan_id":plan.id,"job_id":execution.unwrap_or(plan.job_id)}));
            }
        }
        if !references.is_empty() {
            return Err(AppError::new(
                "plan_in_use",
                format!("Scan has active references: {references:?}"),
            ));
        }
        if let Some(snapshot) = record.snapshot_id {
            self.store.delete_snapshot(&snapshot)?;
            self.snapshots.remove(&snapshot)?;
        }
        self.store.delete("scan", id)?;
        Ok(json!({"released":true,"scan_id":id}))
    }
    fn launch<F>(self: &Arc<Self>, job_id: String, work: F)
    where
        F: FnOnce(Arc<Self>, String, Arc<AtomicBool>) -> Result<String, AppError> + Send + 'static,
    {
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut cancellations) = self.cancellations.lock() {
            cancellations.insert(job_id.clone(), cancel.clone());
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        let engine = self.clone();
        tokio::spawn(async move {
            let permit = engine
                .workers
                .clone()
                .acquire_owned()
                .await
                .expect("worker semaphore is never closed");
            let _ = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    engine.store.update::<Job>("job", &job_id, |j| {
                        if j.state != "cancel_requested" {
                            j.state = "running".into();
                        }
                    })?;
                    work(engine.clone(), job_id.clone(), cancel)
                }))
                .unwrap_or_else(|_| {
                    Err(AppError::new(
                        "worker_panic",
                        "Background worker stopped unexpectedly",
                    ))
                });
                let _ = engine
                    .store
                    .update::<Job>("job", &job_id, |j| match result {
                        Ok(state) => {
                            j.state = state;
                            j.progress.phase = "finished".into();
                        }
                        Err(error) => {
                            j.state = "failed".into();
                            j.error = Some(error);
                        }
                    });
                if let Ok(mut cancellations) = engine.cancellations.lock() {
                    cancellations.remove(&job_id);
                }
                engine.active.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });
    }
}
fn new_job(id: &str, kind: &str, result: Value) -> Job {
    Job {
        id: id.into(),
        kind: kind.into(),
        state: "queued".into(),
        created_at: now_ms(),
        progress: ScanProgress::default(),
        result,
        error: None,
    }
}
fn active_state(state: &str) -> bool {
    matches!(state, "queued" | "running" | "cancel_requested")
}
fn missing(kind: &str) -> AppError {
    AppError::new("not_found", format!("Unknown {kind} identifier"))
}
fn serialization(error: impl std::fmt::Display) -> AppError {
    AppError::new("invalid_arguments", error.to_string())
}
fn poison(error: impl std::fmt::Display) -> AppError {
    AppError::new("internal_error", error.to_string())
}
fn path_argument(args: &Value, key: &str) -> Result<PathBuf, AppError> {
    match args.get(key) {
        Some(Value::String(path)) => Ok(PathBuf::from(path)),
        Some(value) => serde_json::from_value::<NativePath>(value.clone())
            .map_err(serialization)?
            .to_path(),
        None => Err(AppError::new("invalid_arguments", format!("Missing {key}"))),
    }
}
