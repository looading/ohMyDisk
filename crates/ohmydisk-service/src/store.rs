use crate::snapshot_codec;
use ohmydisk_core::{AppError, ItemResult, ItemStatus, Job, Snapshot};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{path::Path, sync::Mutex};

/// One connection belongs to the service. Transactions commit job ownership before
/// any worker may touch the filesystem; FULL synchronous also protects started records.
pub struct Store {
    connection: Mutex<Connection>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SnapshotStorageStats {
    pub codec: &'static str,
    pub stored_bytes: u64,
    pub json_bytes: u64,
}
fn db(error: impl std::fmt::Display) -> AppError {
    AppError::new("state_error", error.to_string())
}
fn encode<T: Serialize>(value: &T) -> Result<String, AppError> {
    serde_json::to_string(value).map_err(db)
}
fn decode<T: DeserializeOwned>(value: &str) -> Result<T, AppError> {
    serde_json::from_str(value).map_err(db)
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        let connection = Connection::open(path).map_err(db)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(10))
            .map_err(db)?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS entities(kind TEXT NOT NULL,id TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(kind,id));
            CREATE TABLE IF NOT EXISTS requests(operation TEXT NOT NULL,request_key TEXT NOT NULL,digest TEXT NOT NULL,response TEXT NOT NULL,PRIMARY KEY(operation,request_key));
            CREATE TABLE IF NOT EXISTS executions(plan_id TEXT PRIMARY KEY,job_id TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS snapshot_blobs(id TEXT PRIMARY KEY,codec TEXT NOT NULL,payload BLOB NOT NULL,json_bytes INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS items(job_id TEXT NOT NULL,node_id TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(job_id,node_id));").map_err(db)?;
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.recover()?;
        Ok(store)
    }

    /// Persist a compressed snapshot without holding the connection during encoding.
    ///
    /// # Errors
    /// Returns `state_error` if encoding or the database write fails.
    pub fn put_snapshot(&self, snapshot: &Snapshot) -> Result<SnapshotStorageStats, AppError> {
        // Snapshot encoding is independent of the connection, so progress and job
        // reads do not wait for JSON serialization or compression under this lock.
        let encoded = snapshot_codec::encode(snapshot)?;
        let json_bytes = i64::try_from(encoded.json_bytes)
            .map_err(|_| db("Snapshot JSON length exceeds SQLite INTEGER range"))?;
        let stats = SnapshotStorageStats {
            codec: snapshot_codec::CODEC,
            stored_bytes: encoded.payload.len() as u64,
            json_bytes: encoded.json_bytes,
        };
        self.connection.lock().map_err(db)?.execute(
            "INSERT INTO snapshot_blobs(id,codec,payload,json_bytes) VALUES(?1,?2,?3,?4) \
             ON CONFLICT(id) DO UPDATE SET codec=excluded.codec,payload=excluded.payload,json_bytes=excluded.json_bytes",
            params![snapshot.id, snapshot_codec::CODEC, encoded.payload, json_bytes],
        ).map_err(db)?;
        Ok(stats)
    }

    /// Load a compressed snapshot, falling back to an untouched legacy JSON row.
    ///
    /// # Errors
    /// Returns `state_error` on a database error, unsupported codec or invalid payload.
    pub fn get_snapshot(&self, id: &str) -> Result<Option<Snapshot>, AppError> {
        enum StoredSnapshot {
            Encoded(String, Vec<u8>, i64),
            Legacy(String),
        }
        let raw = {
            let c = self.connection.lock().map_err(db)?;
            let encoded = c
                .query_row(
                    "SELECT codec,payload,json_bytes FROM snapshot_blobs WHERE id=?1",
                    [id],
                    |row| {
                        Ok(StoredSnapshot::Encoded(
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(db)?;
            match encoded {
                Some(encoded) => Some(encoded),
                None => c
                    .query_row(
                        "SELECT payload FROM entities WHERE kind='snapshot' AND id=?1",
                        [id],
                        |row| Ok(StoredSnapshot::Legacy(row.get(0)?)),
                    )
                    .optional()
                    .map_err(db)?,
            }
        };
        // Both formats decode after the connection guard has been released.
        raw.map(|raw| match raw {
            StoredSnapshot::Encoded(codec, payload, json_bytes) => {
                let json_bytes =
                    u64::try_from(json_bytes).map_err(|_| db("Invalid snapshot JSON length"))?;
                snapshot_codec::decode(&codec, &payload, json_bytes)
            }
            StoredSnapshot::Legacy(payload) => decode(&payload),
        })
        .transpose()
    }

    /// Atomically remove both snapshot representations so legacy data cannot reappear.
    ///
    /// # Errors
    /// Returns `state_error` if either deletion or the transaction commit fails.
    pub fn delete_snapshot(&self, id: &str) -> Result<(), AppError> {
        let mut c = self.connection.lock().map_err(db)?;
        let tx = c.transaction().map_err(db)?;
        tx.execute("DELETE FROM snapshot_blobs WHERE id=?1", [id])
            .map_err(db)?;
        tx.execute("DELETE FROM entities WHERE kind='snapshot' AND id=?1", [id])
            .map_err(db)?;
        tx.commit().map_err(db)
    }

    pub fn get<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<Option<T>, AppError> {
        let c = self.connection.lock().map_err(db)?;
        let raw: Option<String> = c
            .query_row(
                "SELECT payload FROM entities WHERE kind=?1 AND id=?2",
                params![kind, id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        raw.map(|s| decode(&s)).transpose()
    }
    pub fn put<T: Serialize>(&self, kind: &str, id: &str, value: &T) -> Result<(), AppError> {
        self.connection.lock().map_err(db)?.execute("INSERT INTO entities(kind,id,payload) VALUES(?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET payload=excluded.payload",params![kind,id,encode(value)?]).map_err(db)?;
        Ok(())
    }
    pub fn update<T: Serialize + DeserializeOwned>(
        &self,
        kind: &str,
        id: &str,
        update: impl FnOnce(&mut T),
    ) -> Result<T, AppError> {
        let c = self.connection.lock().map_err(db)?;
        let raw: String = c
            .query_row(
                "SELECT payload FROM entities WHERE kind=?1 AND id=?2",
                params![kind, id],
                |r| r.get(0),
            )
            .map_err(db)?;
        let mut value: T = decode(&raw)?;
        update(&mut value);
        c.execute(
            "UPDATE entities SET payload=?3 WHERE kind=?1 AND id=?2",
            params![kind, id, encode(&value)?],
        )
        .map_err(db)?;
        Ok(value)
    }
    pub fn all<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>, AppError> {
        let c = self.connection.lock().map_err(db)?;
        let mut stmt = c
            .prepare("SELECT payload FROM entities WHERE kind=?1 ORDER BY id")
            .map_err(db)?;
        let rows = stmt
            .query_map([kind], |r| r.get::<_, String>(0))
            .map_err(db)?;
        rows.map(|r| decode(&r.map_err(db)?)).collect()
    }
    pub fn delete(&self, kind: &str, id: &str) -> Result<(), AppError> {
        self.connection
            .lock()
            .map_err(db)?
            .execute(
                "DELETE FROM entities WHERE kind=?1 AND id=?2",
                params![kind, id],
            )
            .map_err(db)?;
        Ok(())
    }
    pub fn request(
        &self,
        operation: &str,
        key: &str,
        digest: &str,
    ) -> Result<Option<Value>, AppError> {
        let c = self.connection.lock().map_err(db)?;
        let value: Option<(String, String)> = c
            .query_row(
                "SELECT digest,response FROM requests WHERE operation=?1 AND request_key=?2",
                params![operation, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db)?;
        match value {
            Some((old, response)) if old == digest => Ok(Some(decode(&response)?)),
            Some(_) => Err(AppError::new(
                "request_conflict",
                "request_key was already used with different arguments",
            )),
            None => Ok(None),
        }
    }
    /// Returns (response, newly accepted). A plan claim and job/request registration
    /// are one transaction. A second request for an accepted plan returns its job.
    pub fn accept_job(
        &self,
        operation: &str,
        key: &str,
        digest: &str,
        response: Value,
        job: &Job,
        entries: &[(String, String, Value)],
        claim_plan: Option<&str>,
    ) -> Result<(Value, bool), AppError> {
        let mut c = self.connection.lock().map_err(db)?;
        let tx = c.transaction().map_err(db)?;
        if let Some((old, raw)) = tx
            .query_row(
                "SELECT digest,response FROM requests WHERE operation=?1 AND request_key=?2",
                params![operation, key],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db)?
        {
            if old != digest {
                return Err(AppError::new(
                    "request_conflict",
                    "request_key was already used with different arguments",
                ));
            }
            return Ok((decode(&raw)?, false));
        }
        let existing = if let Some(plan) = claim_plan {
            tx.query_row(
                "SELECT job_id FROM executions WHERE plan_id=?1",
                [plan],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(db)?
        } else {
            None
        };
        if let Some(existing) = existing {
            let value = serde_json::json!({"job_id":existing});
            save_request(&tx, operation, key, digest, &value)?;
            tx.commit().map_err(db)?;
            return Ok((value, false));
        }
        tx.execute(
            "INSERT INTO entities(kind,id,payload) VALUES('job',?1,?2)",
            params![job.id, encode(job)?],
        )
        .map_err(db)?;
        for (kind, id, value) in entries {
            tx.execute(
                "INSERT INTO entities(kind,id,payload) VALUES(?1,?2,?3)",
                params![kind, id, encode(value)?],
            )
            .map_err(db)?;
        }
        if let Some(plan) = claim_plan {
            tx.execute(
                "INSERT INTO executions(plan_id,job_id) VALUES(?1,?2)",
                params![plan, job.id],
            )
            .map_err(db)?;
        }
        save_request(&tx, operation, key, digest, &response)?;
        tx.commit().map_err(db)?;
        Ok((response, true))
    }
    pub fn execution(&self, plan_id: &str) -> Result<Option<String>, AppError> {
        self.connection
            .lock()
            .map_err(db)?
            .query_row(
                "SELECT job_id FROM executions WHERE plan_id=?1",
                [plan_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)
    }
    pub fn record_item(&self, job_id: &str, item: &ItemResult) -> Result<(), AppError> {
        self.connection.lock().map_err(db)?.execute("INSERT INTO items(job_id,node_id,payload) VALUES(?1,?2,?3) ON CONFLICT(job_id,node_id) DO UPDATE SET payload=excluded.payload",params![job_id,item.node_id,encode(item)?]).map_err(db)?;
        Ok(())
    }
    /// Execution may update only members registered before items_ready became true.
    /// An unexpected callback cannot grow the collection behind an issued cursor.
    pub fn update_item(&self, job_id: &str, item: &ItemResult) -> Result<(), AppError> {
        let changed = self
            .connection
            .lock()
            .map_err(db)?
            .execute(
                "UPDATE items SET payload=?3 WHERE job_id=?1 AND node_id=?2",
                params![job_id, item.node_id, encode(item)?],
            )
            .map_err(db)?;
        if changed != 1 {
            return Err(AppError::new(
                "state_error",
                "Cleanup outcome refers to an unregistered item",
            ));
        }
        Ok(())
    }
    pub fn items(&self, job_id: &str) -> Result<Vec<ItemResult>, AppError> {
        let c = self.connection.lock().map_err(db)?;
        let mut stmt = c
            .prepare("SELECT payload FROM items WHERE job_id=?1 ORDER BY node_id")
            .map_err(db)?;
        let rows = stmt
            .query_map([job_id], |r| r.get::<_, String>(0))
            .map_err(db)?;
        rows.map(|r| decode(&r.map_err(db)?)).collect()
    }
    fn recover(&self) -> Result<(), AppError> {
        for mut job in self.all::<Job>("job")? {
            let was_active = matches!(
                job.state.as_str(),
                "queued" | "running" | "cancel_requested"
            );
            let mut changed = was_active;
            if was_active {
                job.state = "interrupted".into();
                job.error = Some(AppError::new(
                    "interrupted",
                    "Service restarted; this job will not be replayed",
                ));
            }
            let mut newly_uncertain = false;
            for mut item in self.items(&job.id)? {
                if item.status == ItemStatus::Started {
                    newly_uncertain = true;
                    item.status = ItemStatus::InDoubt;
                    item.code = Some("in_doubt".into());
                    item.message = Some(
                        "The filesystem operation may have completed before the service stopped"
                            .into(),
                    );
                    self.record_item(&job.id, &item)?;
                }
            }
            if job.kind == "cleanup"
                && (was_active
                    || newly_uncertain
                    || (job.state == "interrupted"
                        && job.result["recovery_invalidated_scans"] != true))
            {
                job.result["needs_rescan"] = Value::Bool(true);
                job.result["recovery_pending"] = Value::Bool(true);
                job.result["refresh_job_id"] = Value::Null;
                job.result["refresh_scan_id"] = Value::Null;
                job.result["recovery_message"] = Value::String(
                    "Cleanup was interrupted. Saved scans may be stale; inspect uncertain outcomes and start a new scan. No deletion will be replayed.".into());
                changed = true;
            }
            if changed {
                self.put("job", &job.id, &job)?;
            }
        }
        Ok(())
    }
}
fn save_request(
    tx: &Transaction<'_>,
    operation: &str,
    key: &str,
    digest: &str,
    response: &Value,
) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO requests(operation,request_key,digest,response) VALUES(?1,?2,?3,?4)",
        params![operation, key, digest, encode(response)?],
    )
    .map_err(db)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        snapshot_codec::tests::snapshot()
    }

    fn assert_same(actual: &Snapshot, expected: &Snapshot) {
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }

    #[test]
    fn compressed_snapshot_round_trip_persists_stats_and_upserts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let store = Store::open(&path).unwrap();
        let mut snapshot = snapshot();
        let stats = store.put_snapshot(&snapshot).unwrap();
        assert_eq!(stats.codec, snapshot_codec::CODEC);
        assert_eq!(
            stats.json_bytes,
            serde_json::to_vec(&snapshot).unwrap().len() as u64
        );
        assert!(stats.stored_bytes < stats.json_bytes);
        {
            let c = store.connection.lock().unwrap();
            let (stored_bytes, json_bytes): (i64, i64) = c
                .query_row(
                    "SELECT length(payload),json_bytes FROM snapshot_blobs WHERE id=?1",
                    [&snapshot.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(stats.stored_bytes, u64::try_from(stored_bytes).unwrap());
            assert_eq!(stats.json_bytes, u64::try_from(json_bytes).unwrap());
        }
        assert_same(
            &store.get_snapshot(&snapshot.id).unwrap().unwrap(),
            &snapshot,
        );
        snapshot.cancelled = false;
        snapshot.nodes[1].logical_bytes = 27;
        store.put_snapshot(&snapshot).unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_same(
            &reopened.get_snapshot(&snapshot.id).unwrap().unwrap(),
            &snapshot,
        );
        assert!(reopened.get_snapshot("unknown-snapshot").unwrap().is_none());
    }

    #[test]
    fn legacy_database_rows_remain_readable_without_migration_or_rewriting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let snapshot = snapshot();
        let payload = serde_json::to_string_pretty(&snapshot).unwrap();
        {
            // The original v0.1 snapshot representation, before Store creates the
            // additional blob table. Preserve even the original JSON formatting.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE entities(kind TEXT NOT NULL,id TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(kind,id));",
            )
            .unwrap();
            c.execute(
                "INSERT INTO entities(kind,id,payload) VALUES('snapshot',?1,?2)",
                params![snapshot.id, payload],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_same(
            &store.get_snapshot(&snapshot.id).unwrap().unwrap(),
            &snapshot,
        );
        let c = store.connection.lock().unwrap();
        let untouched: String = c
            .query_row(
                "SELECT payload FROM entities WHERE kind='snapshot' AND id=?1",
                [&snapshot.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(untouched, payload);
        let new_rows: i64 = c
            .query_row("SELECT count(*) FROM snapshot_blobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(new_rows, 0);
    }

    #[test]
    fn new_snapshot_precedes_legacy_and_delete_removes_both_formats() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("state.sqlite")).unwrap();
        let legacy = snapshot();
        store.put("snapshot", &legacy.id, &legacy).unwrap();
        let mut newer = legacy.clone();
        newer.created_at += 1;
        store.put_snapshot(&newer).unwrap();
        assert_same(&store.get_snapshot(&legacy.id).unwrap().unwrap(), &newer);
        store.delete_snapshot(&legacy.id).unwrap();
        assert!(store.get_snapshot(&legacy.id).unwrap().is_none());
        assert!(
            store
                .get::<Snapshot>("snapshot", &legacy.id)
                .unwrap()
                .is_none()
        );
        store.delete_snapshot(&legacy.id).unwrap();
    }

    #[test]
    fn damaged_or_unknown_new_rows_fail_without_falling_back_to_legacy() {
        use lz4_flex::frame::FrameEncoder;
        use std::io::Write;

        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("state.sqlite")).unwrap();
        let snapshot = snapshot();
        store.put("snapshot", &snapshot.id, &snapshot).unwrap();
        let encoded = snapshot_codec::encode(&snapshot).unwrap();
        let json_bytes = i64::try_from(encoded.json_bytes).unwrap();
        let mut invalid_json = FrameEncoder::new(Vec::new());
        invalid_json.write_all(b"not JSON").unwrap();
        let cases = [
            ("lz4-json-v2", encoded.payload.clone(), json_bytes),
            (snapshot_codec::CODEC, vec![1, 2, 3], json_bytes),
            (
                snapshot_codec::CODEC,
                encoded.payload[..encoded.payload.len() - 1].to_vec(),
                json_bytes,
            ),
            (snapshot_codec::CODEC, invalid_json.finish().unwrap(), 8),
            (snapshot_codec::CODEC, encoded.payload.clone(), -1),
            (snapshot_codec::CODEC, encoded.payload, 1),
        ];
        for (codec, payload, json_bytes) in cases {
            store
                .connection
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO snapshot_blobs(id,codec,payload,json_bytes) VALUES(?1,?2,?3,?4) \
                     ON CONFLICT(id) DO UPDATE SET codec=excluded.codec,payload=excluded.payload,json_bytes=excluded.json_bytes",
                    params![snapshot.id, codec, payload, json_bytes],
                )
                .unwrap();
            assert_eq!(
                store.get_snapshot(&snapshot.id).unwrap_err().code,
                "state_error"
            );
        }
        store
            .connection
            .lock()
            .unwrap()
            .execute("DELETE FROM snapshot_blobs", [])
            .unwrap();
        store
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE entities SET payload='not JSON' WHERE kind='snapshot'",
                [],
            )
            .unwrap();
        assert_eq!(
            store.get_snapshot(&snapshot.id).unwrap_err().code,
            "state_error"
        );
    }

    #[test]
    fn snapshot_storage_preserves_cleanup_records_and_full_wal_durability() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("state.sqlite")).unwrap();
        let job = Job {
            id: "cleanup-job".into(),
            kind: "cleanup".into(),
            state: "running".into(),
            created_at: 123,
            elapsed_ms: None,
            finished_at: None,
            progress: ohmydisk_core::ScanProgress::default(),
            result: serde_json::json!({"items_ready":true}),
            error: None,
        };
        let response = serde_json::json!({"job_id":job.id});
        store
            .accept_job(
                "execute_trash",
                "fixture-request",
                "fixture-digest",
                response.clone(),
                &job,
                &[(
                    "scan".into(),
                    "fixture-scan".into(),
                    serde_json::json!({"complete":true}),
                )],
                Some("fixture-plan"),
            )
            .unwrap();
        let item = ItemResult {
            node_id: "fixture-node".into(),
            path: "/fixture/item".into(),
            status: ItemStatus::Started,
            code: None,
            message: None,
        };
        store.record_item(&job.id, &item).unwrap();
        let snapshot = snapshot();
        store.put_snapshot(&snapshot).unwrap();
        store.get_snapshot(&snapshot.id).unwrap().unwrap();
        store.delete_snapshot(&snapshot.id).unwrap();
        assert_eq!(
            serde_json::to_value(store.get::<Job>("job", &job.id).unwrap().unwrap()).unwrap(),
            serde_json::to_value(job.clone()).unwrap()
        );
        assert_eq!(store.items(&job.id).unwrap()[0].status, ItemStatus::Started);
        assert_eq!(
            store
                .request("execute_trash", "fixture-request", "fixture-digest")
                .unwrap(),
            Some(response)
        );
        assert_eq!(store.execution("fixture-plan").unwrap(), Some(job.id));
        assert_eq!(
            store.get::<Value>("scan", "fixture-scan").unwrap(),
            Some(serde_json::json!({"complete":true}))
        );
        let c = store.connection.lock().unwrap();
        assert_eq!(
            c.query_row::<String, _, _>("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap(),
            "wal"
        );
        assert_eq!(
            c.query_row::<u32, _, _>("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap(),
            2
        );
    }
}
