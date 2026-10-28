use ohmydisk_core::{AppError, ItemResult, ItemStatus, Job};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{path::Path, sync::Mutex};

/// One connection belongs to the service. Transactions commit job ownership before
/// any worker may touch the filesystem; FULL synchronous also protects started records.
pub struct Store {
    connection: Mutex<Connection>,
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
            CREATE TABLE IF NOT EXISTS items(job_id TEXT NOT NULL,node_id TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(job_id,node_id));").map_err(db)?;
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.recover()?;
        Ok(store)
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
