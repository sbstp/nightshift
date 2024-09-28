use std::{collections::BTreeMap, path::Path, sync::LazyLock};

use crate::errors::Result;
use anyhow::Context;
use rusqlite::params;

static MIGRATIONS: LazyLock<BTreeMap<u32, &'static str>> = LazyLock::new(|| {
    let mut m = BTreeMap::new();
    m.insert(1, include_str!("migrations/001_initial_tables.sql"));
    m.insert(2, include_str!("migrations/002_block_compression.sql"));
    m
});

pub struct DatabaseOps {
    pub(crate) db: rusqlite::Connection,
}

impl DatabaseOps {
    pub fn open(path: &Path, key: String) -> anyhow::Result<Self> {
        let mut db = rusqlite::Connection::open(path).context("open")?;
        set_cipher_key(&db, key)?;
        migrate_database(&mut db)?;
        Ok(DatabaseOps { db })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let mut db = rusqlite::Connection::open_in_memory().context("open")?;
        migrate_database(&mut db)?;
        Ok(DatabaseOps { db })
    }

    pub fn with_read_tx<T, F>(&mut self, scope: F) -> Result<T>
    where
        F: FnOnce(&mut rusqlite::Transaction) -> Result<T>,
    {
        let mut tx = self.db.transaction()?;
        scope(&mut tx)
    }

    pub fn with_write_tx<T, F>(&mut self, scope: F) -> Result<T>
    where
        F: FnOnce(&mut rusqlite::Transaction) -> Result<T>,
    {
        let mut tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let val = scope(&mut tx)?;
        tx.commit()?;
        Ok(val)
    }
    pub fn vacuum(&mut self) -> anyhow::Result<()> {
        self.db.execute("VACUUM;", params![])?;
        Ok(())
    }
}

fn set_cipher_key(db: &rusqlite::Connection, key: String) -> anyhow::Result<()> {
    db.pragma_update(None, "key", key).context("pragma")?;
    match db
        .prepare("SELECT count(*) FROM sqlite_master")
        .and_then(|mut stmt| stmt.query(params![]).map(|_| ()))
    {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ffi::ErrorCode::NotADatabase => {
            anyhow::bail!("Invalid key");
        }
        Err(e) => anyhow::bail!("SQLite error: {e}"),
    }
}

pub(crate) fn migrate_database(db: &mut rusqlite::Connection) -> anyhow::Result<()> {
    migrate_database_inner(db).context("Migration error: rolled back all changes")
}

fn migrate_database_inner(db: &mut rusqlite::Connection) -> anyhow::Result<()> {
    db.execute_batch(include_str!("pragmas.sql"))?;

    let tx = db.transaction()?;
    let current_version: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let mut last_version = current_version;
    for (&version, &migration) in &*MIGRATIONS {
        if version > current_version {
            log::info!(
                "Running migration #{} because current_version is #{}",
                version,
                current_version
            );
            tx.execute_batch(migration)
                .with_context(|| format!("Error running migration #{}", version,))?;
        } else {
            log::info!(
                "Skipping migration #{} because current version is #{}",
                version,
                current_version
            );
        }

        last_version = version;
    }
    if last_version > current_version {
        log::info!("Updating current_version to #{}", last_version);
        tx.pragma_update(None, "user_version", last_version)?;
    }
    tx.commit()?;
    Ok(())
}
