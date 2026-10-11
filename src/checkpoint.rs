//! Transactional native workspace checkpoints. Portable project files stay JSON.
use crate::{
    engine::{CommitSink, EngineError},
    project::Project,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    path::Path,
    sync::Mutex,
};

const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Status {
    pub configured: bool,
    pub saved_revision: Option<u64>,
    pub available: bool,
}

#[derive(Debug)]
pub struct Checkpoint {
    connection: Mutex<Connection>,
    status: Mutex<Status>,
    // A separate permanent lock also protects startup before SQLite opens.
    _owner: File,
}

impl Checkpoint {
    pub fn open(path: &Path) -> anyhow::Result<(Self, Option<(Project, u64)>)> {
        let path = if path.exists() {
            path.canonicalize()?
        } else {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            parent.canonicalize()?.join(
                path.file_name()
                    .ok_or_else(|| anyhow::anyhow!("workspace needs a filename"))?,
            )
        };
        let mut lock = path.as_os_str().to_os_string();
        lock.push(".lock");
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock)?;
        owner
            .try_lock()
            .map_err(|_| anyhow::anyhow!("native workspace is already open in another process"))?;
        let existing = path.metadata().map(|m| m.len() > 0).unwrap_or(false);
        let connection = Connection::open(&path)?;
        if existing {
            let table: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='tatara_checkpoint')", [], |r| r.get(0))?;
            anyhow::ensure!(table, "file is not a Tatara native workspace");
            let integrity: String = connection.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
            anyhow::ensure!(integrity == "ok", "native workspace integrity check failed");
        }
        connection.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA locking_mode=EXCLUSIVE;",
        )?;
        if !existing {
            connection.execute_batch("CREATE TABLE tatara_checkpoint (id INTEGER PRIMARY KEY CHECK(id=1), version INTEGER NOT NULL, revision TEXT NOT NULL, project TEXT NOT NULL)")?;
        }
        let row: Option<(u32, String, String)> = connection
            .query_row(
                "SELECT version, revision, project FROM tatara_checkpoint WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        anyhow::ensure!(
            !existing || row.is_some(),
            "native workspace has no committed checkpoint"
        );
        let archived = row
            .map(|(version, revision, text)| -> anyhow::Result<_> {
                anyhow::ensure!(version == 1, "unsupported native workspace version");
                anyhow::ensure!(text.len() <= MAX_BYTES, "native workspace exceeds 64 MiB");
                Ok((
                    serde_json::from_str::<Project>(&text)?,
                    revision.parse::<u64>()?,
                ))
            })
            .transpose()?;
        let saved_revision = archived.as_ref().map(|(_, revision)| *revision);
        Ok((
            Self {
                connection: Mutex::new(connection),
                status: Mutex::new(Status {
                    configured: true,
                    saved_revision,
                    available: true,
                }),
                _owner: owner,
            },
            archived,
        ))
    }

    pub fn status(&self) -> Status {
        self.status.lock().expect("checkpoint status lock").clone()
    }

    fn save(&self, project: &Project, revision: u64) -> Result<(), EngineError> {
        let text = serde_json::to_string(project).map_err(|e| EngineError::new(e.to_string()))?;
        if text.len() > MAX_BYTES {
            return Err(EngineError::new(
                "native workspace exceeds 64 MiB; edit was not applied",
            ));
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| EngineError::new("native workspace is unavailable"))?;
        let transaction = connection
            .transaction()
            .map_err(|e| EngineError::new(format!("could not save native workspace: {e}")))?;
        transaction.execute("INSERT INTO tatara_checkpoint(id,version,revision,project) VALUES(1,1,?1,?2) ON CONFLICT(id) DO UPDATE SET version=1,revision=excluded.revision,project=excluded.project",params![revision.to_string(), text]).map_err(|e| EngineError::new(format!("could not save native workspace: {e}")))?;
        transaction
            .commit()
            .map_err(|e| EngineError::new(format!("could not commit native workspace: {e}")))?;
        Ok(())
    }
}

impl CommitSink for Checkpoint {
    fn persist(&self, project: &Project, revision: u64) -> Result<(), EngineError> {
        if !self.status().available {
            return Err(EngineError::new(
                "native workspace is unavailable; restart after resolving storage access",
            ));
        }
        let result = self.save(project, revision);
        let mut status = self.status.lock().expect("checkpoint status lock");
        match &result {
            Ok(()) => status.saved_revision = Some(revision),
            Err(_) => status.available = false,
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CommandBatch, Editor};
    use std::sync::atomic::{AtomicU64, Ordering};
    fn workspace() -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let folder = std::env::temp_dir().join(format!(
            "tatara-checkpoint-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&folder).unwrap();
        folder.join("project.tatara.db")
    }
    #[test]
    fn checkpoint_transactions_reopen_recipe_and_exclude_other_writers() {
        let path = workspace();
        let (checkpoint, archived) = Checkpoint::open(&path).unwrap();
        assert!(archived.is_none());
        assert!(Checkpoint::open(&path).is_err());
        let mut ed = Editor::new();
        let batch: CommandBatch = serde_json::from_value(serde_json::json!({"commands":[{"op":"add","name":"Saved box","primitive":{"kind":"cube"}}],"actor":{"id":"artist","name":"Saved Artist"}})).unwrap();
        ed.apply(&batch).unwrap();
        checkpoint.persist(&ed.project(), 41).unwrap();
        assert_eq!(checkpoint.status().saved_revision, Some(41));
        drop(checkpoint);
        let (checkpoint, saved) = Checkpoint::open(&path).unwrap();
        let (project, revision) = saved.unwrap();
        assert_eq!(project, ed.project());
        assert_eq!(revision, 41);
        let restored = Editor::from_checkpoint(project, revision).unwrap();
        assert_eq!(restored.scene().revision, 42);
        assert!(!restored.can_undo());
        assert!(!restored.can_redo());
        assert!(restored.ensure_revision(Some(41)).unwrap_err().stale);
        assert_eq!(
            restored.history().0[0].actor.as_ref().unwrap().name,
            "Saved Artist"
        );
        drop(checkpoint);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn rejected_checkpoint_write_preserves_the_previous_transaction() {
        let path = workspace();
        let (checkpoint, _) = Checkpoint::open(&path).unwrap();
        let project = Editor::new().project();
        checkpoint.persist(&project, 7).unwrap();
        checkpoint
            .connection
            .lock()
            .unwrap()
            .execute_batch("PRAGMA query_only=ON")
            .unwrap();
        assert!(checkpoint.persist(&project, 8).is_err());
        assert!(!checkpoint.status().available);
        assert_eq!(checkpoint.status().saved_revision, Some(7));
        drop(checkpoint);
        let (checkpoint, saved) = Checkpoint::open(&path).unwrap();
        assert_eq!(saved.unwrap().1, 7);
        drop(checkpoint);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn corrupted_native_workspace_is_refused_without_overwriting_it() {
        let path = workspace();
        std::fs::write(&path, b"corrupted checkpoint").unwrap();
        assert!(Checkpoint::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupted checkpoint");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn invalid_recipe_and_exhausted_revision_do_not_replace_the_archive() {
        let path = workspace();
        let (checkpoint, _) = Checkpoint::open(&path).unwrap();
        checkpoint
            .persist(&crate::engine::Editor::new().project(), u64::MAX)
            .unwrap();
        drop(checkpoint);
        assert!(crate::server::persistent_state(None, &path).is_err());
        let (checkpoint, archived) = Checkpoint::open(&path).unwrap();
        assert_eq!(archived.unwrap().1, u64::MAX);
        checkpoint
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE tatara_checkpoint SET revision='9', project='{}' WHERE id=1",
                [],
            )
            .unwrap();
        drop(checkpoint);
        assert!(crate::server::persistent_state(None, &path).is_err());
        let connection = Connection::open(&path).unwrap();
        let text: String = connection
            .query_row("SELECT project FROM tatara_checkpoint", [], |r| r.get(0))
            .unwrap();
        assert_eq!(text, "{}");
        drop(connection);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
