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
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(storage)?;
        if !matches!(version, 0 | 1) {
            return Err(EngineError::Recovery {
                detail: format!("This operational database uses unsupported schema version {version}. Keep its data and open it with a compatible Velmux version."),
            });
        }
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY,submission_key TEXT NOT NULL UNIQUE,request TEXT NOT NULL,record TEXT NOT NULL,state TEXT NOT NULL,created_ms INTEGER NOT NULL,updated_ms INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);").map_err(storage)?;
        if version == 0 {
            connection
                .pragma_update(None, "user_version", 1)
                .map_err(storage)?;
        }
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

#[cfg(test)]
mod schema_tests {
    use super::*;
    use std::path::PathBuf;

    struct OwnedRoot(PathBuf);
    impl OwnedRoot {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("velmux-store-schema-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn database(&self) -> PathBuf {
            self.0.join("operations.sqlite3")
        }
    }
    impl Drop for OwnedRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn new_store_initializes_the_supported_schema_version() {
        let root = OwnedRoot::new();
        let store = Store::open(&root.0).unwrap();
        let connection = store.connection.lock().unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let tables: i64 = connection
            .query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('jobs','settings')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tables, 2);
    }

    #[test]
    fn existing_zero_and_one_versions_preserve_operational_payloads_and_settings() {
        for version in [0, 1] {
            let root = OwnedRoot::new();
            let connection = Connection::open(root.database()).unwrap();
            // Keep these persisted payloads opaque: schema initialization must
            // not rewrite an accepted request or a job's recorded state.
            let request = r#"{"submission_key":"legacy-submission","accepted":"immutable"}"#;
            let record = r#"{"state":"Paused","source_name":"original.wav"}"#;
            connection.execute_batch("CREATE TABLE jobs(id TEXT PRIMARY KEY,submission_key TEXT NOT NULL UNIQUE,request TEXT NOT NULL,record TEXT NOT NULL,state TEXT NOT NULL,created_ms INTEGER NOT NULL,updated_ms INTEGER NOT NULL);
                CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);").unwrap();
            connection
                .execute(
                    "INSERT INTO jobs VALUES(?1,?2,?3,?4,'paused',10,20)",
                    params!["legacy-job", "legacy-submission", request, record],
                )
                .unwrap();
            connection
                .execute("INSERT INTO settings VALUES('appearance','dark')", [])
                .unwrap();
            connection
                .pragma_update(None, "user_version", version)
                .unwrap();
            drop(connection);

            let store = Store::open(&root.0).unwrap();
            assert_eq!(
                store.setting("appearance").unwrap().as_deref(),
                Some("dark")
            );
            let connection = store.connection.lock().unwrap();
            let retained: (String, String, String, String, i64, i64) = connection
                .query_row("SELECT submission_key,request,record,state,created_ms,updated_ms FROM jobs WHERE id='legacy-job'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
                .unwrap();
            assert_eq!(
                retained,
                (
                    "legacy-submission".into(),
                    request.into(),
                    record.into(),
                    "paused".into(),
                    10,
                    20
                )
            );
            let actual_version: i64 = connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(actual_version, 1);
        }
    }

    #[test]
    fn future_schema_refusal_preserves_its_marker_content_and_journal_mode() {
        let root = OwnedRoot::new();
        let connection = Connection::open(root.database()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE future_payload(key TEXT PRIMARY KEY,content BLOB NOT NULL);
            INSERT INTO future_payload VALUES('unfinished',x'001122ff');
            PRAGMA user_version=7;",
            )
            .unwrap();
        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        drop(connection);

        assert!(
            matches!(Store::open(&root.0), Err(EngineError::Recovery { detail }) if detail.contains("schema version 7"))
        );

        let connection = Connection::open(root.database()).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        let payload: Vec<u8> = connection
            .query_row(
                "SELECT content FROM future_payload WHERE key='unfinished'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(payload, [0, 0x11, 0x22, 0xff]);
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            tables, 1,
            "Refusal must not initialize known-version tables"
        );
        let retained_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(retained_mode, journal_mode);
    }
}
