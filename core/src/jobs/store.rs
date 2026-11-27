use super::{AcceptedRequest, JobRecord, JobState};
use crate::EngineError;
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    fs::{File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
    sync::Mutex,
    time::Duration,
};

pub(super) struct Store {
    connection: Mutex<Connection>,
    _owner: File,
}
fn storage(error: impl std::fmt::Display) -> EngineError {
    EngineError::Storage {
        detail: error.to_string(),
    }
}
impl Store {
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        std::fs::create_dir_all(root).map_err(storage)?;
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.join("engine.lock"))
            .map_err(storage)?;
        if unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(EngineError::Internal {
                detail: "Another engine already owns this operational store.".into(),
            });
        }
        let connection = Connection::open(root.join("operations.sqlite3")).map_err(storage)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(storage)?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY,submission_key TEXT NOT NULL UNIQUE,request TEXT NOT NULL,record TEXT NOT NULL,state TEXT NOT NULL,created_ms INTEGER NOT NULL,updated_ms INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            PRAGMA user_version=1;").map_err(storage)?;
        Ok(Self {
            connection: Mutex::new(connection),
            _owner: owner,
        })
    }
    pub fn list(&self) -> Result<Vec<JobRecord>, EngineError> {
        let connection = self.connection.lock().map_err(storage)?;
        let mut statement = connection
            .prepare("SELECT record FROM jobs ORDER BY created_ms DESC,id")
            .map_err(storage)?;
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(storage)?;
        rows.map(|r| serde_json::from_str(&r.map_err(storage)?).map_err(storage))
            .collect()
    }
    pub fn get(&self, id: &str) -> Result<JobRecord, EngineError> {
        let connection = self.connection.lock().map_err(storage)?;
        let value: Option<String> = connection
            .query_row("SELECT record FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(storage)?;
        serde_json::from_str(&value.ok_or_else(|| EngineError::Configuration {
            detail: "This job no longer exists.".into(),
        })?)
        .map_err(storage)
    }
    pub fn request(&self, id: &str) -> Result<AcceptedRequest, EngineError> {
        let connection = self.connection.lock().map_err(storage)?;
        let value: String = connection
            .query_row("SELECT request FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .map_err(storage)?;
        serde_json::from_str(&value).map_err(storage)
    }
    pub fn by_submission(
        &self,
        key: &str,
    ) -> Result<Option<(JobRecord, AcceptedRequest)>, EngineError> {
        let connection = self.connection.lock().map_err(storage)?;
        let value: Option<(String, String)> = connection
            .query_row(
                "SELECT record,request FROM jobs WHERE submission_key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        value
            .map(|(record, request)| {
                Ok((
                    serde_json::from_str(&record).map_err(storage)?,
                    serde_json::from_str(&request).map_err(storage)?,
                ))
            })
            .transpose()
    }
    pub fn insert(&self, record: &JobRecord, request: &AcceptedRequest) -> Result<(), EngineError> {
        self.connection.lock().map_err(storage)?.execute("INSERT INTO jobs(id,submission_key,request,record,state,created_ms,updated_ms) VALUES(?1,?2,?3,?4,?5,?6,?6)",params![record.id,request.submission_key,serde_json::to_string(request).map_err(storage)?,serde_json::to_string(record).map_err(storage)?,record.state.as_str(),record.created_ms]).map_err(storage)?;
        Ok(())
    }
    pub fn update(&self, record: &JobRecord) -> Result<(), EngineError> {
        let count = self
            .connection
            .lock()
            .map_err(storage)?
            .execute(
                "UPDATE jobs SET record=?1,state=?2,updated_ms=?3 WHERE id=?4",
                params![
                    serde_json::to_string(record).map_err(storage)?,
                    record.state.as_str(),
                    super::now_ms(),
                    record.id
                ],
            )
            .map_err(storage)?;
        if count != 1 {
            return Err(EngineError::Internal {
                detail: "The worker lost its job record.".into(),
            });
        }
        Ok(())
    }
    pub fn remove(&self, id: &str) -> Result<(), EngineError> {
        self.connection
            .lock()
            .map_err(storage)?
            .execute("DELETE FROM jobs WHERE id=?1", [id])
            .map_err(storage)?;
        Ok(())
    }
    pub fn next_queued(&self) -> Result<Option<String>, EngineError> {
        self.connection
            .lock()
            .map_err(storage)?
            .query_row(
                "SELECT id FROM jobs WHERE state=?1 ORDER BY created_ms,id LIMIT 1",
                [JobState::Queued.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)
    }
    pub fn setting(&self, key: &str) -> Result<Option<String>, EngineError> {
        self.connection
            .lock()
            .map_err(storage)?
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(storage)
    }
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), EngineError> {
        self.connection.lock().map_err(storage)?.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[key,value]).map_err(storage)?;
        Ok(())
    }
}
