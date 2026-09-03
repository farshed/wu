pub mod kvp;
pub mod query;

// Re-export
pub use anyhow;
use anyhow::Context as _;
pub use gpui;
use gpui::{App, AppContext, Global};
pub use indoc::indoc;
pub use inventory;
pub use paths::database_dir;
pub use sqlez;
pub use sqlez_macros;
pub use uuid;

pub use release_channel::RELEASE_CHANNEL;
use release_channel::ReleaseChannel;
use sqlez::connection::SqliteError;
use sqlez::domain::Migrator;
use sqlez::migrations::MigrationChangedError;
use sqlez::thread_safe_connection::ThreadSafeConnection;
use sqlez_macros::sql;
use std::fs::create_dir_all;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{LazyLock, atomic::Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use util::ResultExt;
use wu_env_vars::ZED_STATELESS;

/// A migration registered via `static_connection!` and collected at link time.
pub struct DomainMigration {
    pub name: &'static str,
    pub migrations: &'static [&'static str],
    pub dependencies: &'static [&'static str],
    pub should_allow_migration_change: fn(usize, &str, &str) -> bool,
}

inventory::collect!(DomainMigration);

/// The shared database connection backing all domain-specific DB wrappers.
/// Set as a GPUI global per-App. Falls back to a shared LazyLock if not set.
pub struct AppDatabase(pub ThreadSafeConnection);

impl Global for AppDatabase {}

/// Migrator that runs all inventory-registered domain migrations.
pub struct AppMigrator;

impl Migrator for AppMigrator {
    fn migrate(connection: &sqlez::connection::Connection) -> anyhow::Result<()> {
        let registrations: Vec<&DomainMigration> = inventory::iter::<DomainMigration>().collect();
        let sorted = topological_sort(&registrations);
        for reg in &sorted {
            let mut should_allow = reg.should_allow_migration_change;
            connection.migrate(reg.name, reg.migrations, &mut should_allow)?;
        }
        Ok(())
    }
}

impl AppDatabase {
    /// Opens the production database and runs all inventory-registered
    /// migrations in dependency order.
    pub fn new() -> Self {
        let db_dir = database_dir();
        let connection = gpui::block_on(open_db::<AppMigrator>(db_dir, *RELEASE_CHANNEL));
        Self(connection)
    }

    /// Creates a new in-memory database with a unique name and runs all
    /// inventory-registered migrations in dependency order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_new() -> Self {
        let name = format!("test-db-{}", uuid::Uuid::new_v4());
        let connection = gpui::block_on(open_test_db::<AppMigrator>(&name));
        Self(connection)
    }

    /// Returns the per-App connection if set, otherwise falls back to
    /// the shared LazyLock.
    pub fn global(cx: &App) -> &ThreadSafeConnection {
        #[allow(unreachable_code)]
        if let Some(db) = cx.try_global::<Self>() {
            return &db.0;
        } else {
            #[cfg(any(feature = "test-support", test))]
            return &TEST_APP_DATABASE.0;

            panic!("database not initialized")
        }
    }
}

fn topological_sort<'a>(registrations: &[&'a DomainMigration]) -> Vec<&'a DomainMigration> {
    let mut sorted: Vec<&DomainMigration> = Vec::new();
    let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();

    fn visit<'a>(
        name: &str,
        registrations: &[&'a DomainMigration],
        sorted: &mut Vec<&'a DomainMigration>,
        visited: &mut std::collections::HashSet<&'a str>,
    ) {
        if visited.contains(name) {
            return;
        }
        if let Some(reg) = registrations.iter().find(|r| r.name == name) {
            for dep in reg.dependencies {
                visit(dep, registrations, sorted, visited);
            }
            visited.insert(reg.name);
            sorted.push(reg);
        }
    }

    for reg in registrations {
        visit(reg.name, registrations, &mut sorted, &mut visited);
    }
    sorted
}

/// Shared fallback `AppDatabase` used when no per-App global is set.
#[cfg(any(test, feature = "test-support"))]
static TEST_APP_DATABASE: LazyLock<AppDatabase> = LazyLock::new(AppDatabase::test_new);

const CONNECTION_INITIALIZE_QUERY: &str = sql!(
    PRAGMA foreign_keys=TRUE;
);

const DB_INITIALIZE_QUERY: &str = sql!(
    PRAGMA busy_timeout=500;
    PRAGMA journal_mode=WAL;
    PRAGMA case_sensitive_like=TRUE;
    PRAGMA synchronous=NORMAL;
);

const FALLBACK_DB_NAME: &str = "FALLBACK_MEMORY_DB";

const DB_FILE_NAME: &str = "db.sqlite";

pub static ALL_FILE_DB_FAILED: LazyLock<AtomicBool> = LazyLock::new(|| AtomicBool::new(false));

/// Serializes opening on-disk databases so two openers cannot both move a broken
/// database aside, which would move the fresh replacement created by the first.
static DB_OPEN_LOCK: futures::lock::Mutex<()> = futures::lock::Mutex::new(());

/// A type that can be used as a database scope for path construction.
pub trait DbScope {
    fn scope_name(&self) -> &str;
}

impl DbScope for ReleaseChannel {
    fn scope_name(&self) -> &str {
        self.dev_name()
    }
}

/// A database scope shared across all release channels.
pub struct GlobalDbScope;

impl DbScope for GlobalDbScope {
    fn scope_name(&self) -> &str {
        "global"
    }
}

/// Returns the path to the `AppDatabase` SQLite file for the given scope
/// under `db_dir`.
pub fn db_path(db_dir: &Path, scope: impl DbScope) -> PathBuf {
    db_dir
        .join(format!("0-{}", scope.scope_name()))
        .join(DB_FILE_NAME)
}

/// Open or create a database at the given directory path.
/// If opening (including running migrations) fails, the database file is moved to a
/// timestamped backup next to it and a fresh one is created at the original path. If that
/// also fails, a shared in memory db is created and `ALL_FILE_DB_FAILED` is set so that the
/// user can be notified.
pub async fn open_db<M: Migrator + 'static>(
    db_dir: &Path,
    scope: impl DbScope,
) -> ThreadSafeConnection {
    if *ZED_STATELESS {
        return open_fallback_db::<M>().await;
    }

    let db_path = db_path(db_dir, scope);

    if let Some(connection) = open_or_recreate_main_db::<M>(&db_path).await {
        return connection;
    }

    // Set another static ref so that we can escalate the notification
    ALL_FILE_DB_FAILED.store(true, Ordering::Release);

    // If still failed, create an in memory db with a known name
    open_fallback_db::<M>().await
}

async fn open_or_recreate_main_db<M: Migrator>(db_path: &Path) -> Option<ThreadSafeConnection> {
    let _open_guard = DB_OPEN_LOCK.lock().await;

    if let Some(parent) = db_path.parent() {
        create_dir_all(parent)
            .context("Could not create db directory")
            .log_err()?;
    }

    let open_error = match open_main_db::<M>(db_path).await {
        Ok(connection) => return Some(connection),
        Err(error) => error,
    };

    // Transient failures such as a lock held by another process must not move a
    // healthy database aside; only an unreadable file or an unusable schema does.
    if !is_unrecoverable_db_error(&open_error) {
        log::error!(
            "Could not open database {}: {open_error:#}",
            db_path.display()
        );
        return None;
    }

    let backup_path = match move_db_to_backup(db_path) {
        Ok(backup_path) => backup_path,
        Err(backup_error) => {
            log::error!(
                "Could not open database {}: {open_error:#}. Moving it aside also failed: {backup_error:#}",
                db_path.display()
            );
            return None;
        }
    };

    log::error!(
        "Could not open database {}: {open_error:#}. The old database was moved to {} and a fresh database is being created in its place",
        db_path.display(),
        backup_path.display()
    );

    open_main_db::<M>(db_path).await.log_err()
}

fn is_unrecoverable_db_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| {
            cause.downcast_ref::<MigrationChangedError>().is_some()
                || cause
                    .downcast_ref::<SqliteError>()
                    .is_some_and(SqliteError::is_corruption)
        })
}

/// Renames the database file and its `-wal` / `-shm` sidecars to a timestamped backup name
/// next to the original, so the backup stays openable as a normal SQLite database.
fn move_db_to_backup(db_path: &Path) -> anyhow::Result<PathBuf> {
    let file_name = db_path
        .file_name()
        .with_context(|| format!("Database path {} has no file name", db_path.display()))?
        .to_string_lossy()
        .into_owned();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before the unix epoch")?
        .as_secs();
    let mut backup_file_name = format!("{file_name}.backup-{timestamp}");
    let mut attempt = 1;
    while db_path.with_file_name(&backup_file_name).exists() {
        backup_file_name = format!("{file_name}.backup-{timestamp}-{attempt}");
        attempt += 1;
    }
    let backup_path = db_path.with_file_name(&backup_file_name);

    // Sidecars go first: a fresh database next to an orphaned old `-wal` would
    // replay stale frames into itself.
    let mut moved_sidecars = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let sidecar_path = db_path.with_file_name(format!("{file_name}{suffix}"));
        if !sidecar_path.exists() {
            continue;
        }
        let sidecar_backup_path = db_path.with_file_name(format!("{backup_file_name}{suffix}"));
        if let Err(error) = std::fs::rename(&sidecar_path, &sidecar_backup_path) {
            restore_moved_files(&moved_sidecars);
            return Err(error).with_context(|| {
                format!(
                    "Could not move {} to {}",
                    sidecar_path.display(),
                    sidecar_backup_path.display()
                )
            });
        }
        moved_sidecars.push((sidecar_path, sidecar_backup_path));
    }

    if let Err(error) = std::fs::rename(db_path, &backup_path) {
        restore_moved_files(&moved_sidecars);
        return Err(error).with_context(|| {
            format!(
                "Could not move {} to {}",
                db_path.display(),
                backup_path.display()
            )
        });
    }

    Ok(backup_path)
}

fn restore_moved_files(moved: &[(PathBuf, PathBuf)]) {
    for (original, backup) in moved {
        if let Err(error) = std::fs::rename(backup, original) {
            log::error!(
                "Could not move {} back to {}: {error}",
                backup.display(),
                original.display()
            );
        }
    }
}

async fn open_main_db<M: Migrator>(db_path: &Path) -> anyhow::Result<ThreadSafeConnection> {
    log::trace!("Opening database {}", db_path.display());
    ThreadSafeConnection::builder::<M>(db_path.to_string_lossy().as_ref(), true)
        .with_db_initialization_query(DB_INITIALIZE_QUERY)
        .with_connection_initialize_query(CONNECTION_INITIALIZE_QUERY)
        .build()
        .await
}

async fn open_fallback_db<M: Migrator>() -> ThreadSafeConnection {
    log::warn!("Opening fallback in-memory database");
    ThreadSafeConnection::builder::<M>(FALLBACK_DB_NAME, false)
        .with_db_initialization_query(DB_INITIALIZE_QUERY)
        .with_connection_initialize_query(CONNECTION_INITIALIZE_QUERY)
        .build()
        .await
        .expect(
            "Fallback in memory database failed. Likely initialization queries or migrations have fundamental errors",
        )
}

#[cfg(any(test, feature = "test-support"))]
pub async fn open_test_db<M: Migrator>(db_name: &str) -> ThreadSafeConnection {
    use sqlez::thread_safe_connection::locking_queue;

    ThreadSafeConnection::builder::<M>(db_name, false)
        .with_db_initialization_query(DB_INITIALIZE_QUERY)
        .with_connection_initialize_query(CONNECTION_INITIALIZE_QUERY)
        // Serialize queued writes via a mutex and run them synchronously
        .with_write_queue_constructor(locking_queue())
        .build()
        .await
        .unwrap()
}

/// Implements a basic DB wrapper for a given domain
///
/// Arguments:
/// - type of connection wrapper
/// - dependencies, whose migrations should be run prior to this domain's migrations
#[macro_export]
macro_rules! static_connection {
    ($t:ident, [ $($d:ty),* ]) => {
        impl ::std::ops::Deref for $t {
            type Target = $crate::sqlez::thread_safe_connection::ThreadSafeConnection;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl ::std::clone::Clone for $t {
            fn clone(&self) -> Self {
                $t(self.0.clone())
            }
        }

        impl $t {
            /// Returns an instance backed by the per-App database if set,
            /// or the shared fallback connection otherwise.
            pub fn global(cx: &$crate::gpui::App) -> Self {
                $t($crate::AppDatabase::global(cx).clone())
            }

            #[cfg(any(test, feature = "test-support"))]
            pub async fn open_test_db(name: &'static str) -> Self {
                $t($crate::open_test_db::<$t>(name).await)
            }
        }

        $crate::inventory::submit! {
            $crate::DomainMigration {
                name: <$t as $crate::sqlez::domain::Domain>::NAME,
                migrations: <$t as $crate::sqlez::domain::Domain>::MIGRATIONS,
                dependencies: &[$(<$d as $crate::sqlez::domain::Domain>::NAME),*],
                should_allow_migration_change: <$t as $crate::sqlez::domain::Domain>::should_allow_migration_change,
            }
        }
    }
}

pub fn write_and_log<F>(cx: &App, db_write: impl FnOnce() -> F + Send + 'static)
where
    F: Future<Output = anyhow::Result<()>> + Send,
{
    cx.background_spawn(async move { db_write().await.log_err() })
        .detach()
}

#[cfg(test)]
mod tests {
    use std::thread;

    use sqlez::domain::Domain;
    use sqlez_macros::sql;

    use crate::{db_path, open_db, open_or_recreate_main_db};

    // Test bad migration panics
    #[gpui::test]
    #[should_panic]
    async fn test_bad_migration_panics() {
        enum BadDB {}

        impl Domain for BadDB {
            const NAME: &str = "db_tests";
            const MIGRATIONS: &[&str] = &[
                sql!(CREATE TABLE test(value);),
                // failure because test already exists
                sql!(CREATE TABLE test(value);),
            ];
        }

        let tempdir = tempfile::Builder::new()
            .prefix("DbTests")
            .tempdir()
            .unwrap();
        let _bad_db = open_db::<BadDB>(tempdir.path(), release_channel::ReleaseChannel::Dev).await;
    }

    /// A migration that merely fails must not move the database aside.
    #[gpui::test]
    async fn test_failed_migration_keeps_db_file(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();

        enum BadDB {}

        impl Domain for BadDB {
            const NAME: &str = "db_tests";
            const MIGRATIONS: &[&str] = &[
                sql!(CREATE TABLE test(value);),
                sql!(CREATE TABLE test(value);),
            ];
        }

        let tempdir = tempfile::Builder::new()
            .prefix("DbTests")
            .tempdir()
            .unwrap();
        let db_path = db_path(tempdir.path(), release_channel::ReleaseChannel::Dev);
        assert!(open_or_recreate_main_db::<BadDB>(&db_path).await.is_none());
        assert!(db_path.exists());
        let backups: Vec<_> = std::fs::read_dir(db_path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains("backup"))
            .collect();
        assert!(backups.is_empty(), "unexpected backups: {backups:?}");
    }

    /// Test that DB exists but corrupted (causing recreate)
    #[gpui::test]
    async fn test_db_corruption(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();

        enum CorruptedDB {}

        impl Domain for CorruptedDB {
            const NAME: &str = "db_tests";
            const MIGRATIONS: &[&str] = &[sql!(CREATE TABLE test(value);)];
        }

        enum GoodDB {}

        impl Domain for GoodDB {
            const NAME: &str = "db_tests"; //Notice same name
            const MIGRATIONS: &[&str] = &[sql!(CREATE TABLE test2(value);)];
        }

        let tempdir = tempfile::Builder::new()
            .prefix("DbTests")
            .tempdir()
            .unwrap();
        {
            let corrupt_db =
                open_db::<CorruptedDB>(tempdir.path(), release_channel::ReleaseChannel::Dev).await;
            assert!(corrupt_db.persistent());
        }

        let good_db = open_db::<GoodDB>(tempdir.path(), release_channel::ReleaseChannel::Dev).await;
        assert!(
            good_db.select_row::<usize>("SELECT * FROM test2").unwrap()()
                .unwrap()
                .is_none()
        );
    }

    /// Test that DB exists but corrupted (causing recreate)
    #[gpui::test(iterations = 30)]
    async fn test_simultaneous_db_corruption(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();

        enum CorruptedDB {}

        impl Domain for CorruptedDB {
            const NAME: &str = "db_tests";

            const MIGRATIONS: &[&str] = &[sql!(CREATE TABLE test(value);)];
        }

        enum GoodDB {}

        impl Domain for GoodDB {
            const NAME: &str = "db_tests"; //Notice same name
            const MIGRATIONS: &[&str] = &[sql!(CREATE TABLE test2(value);)]; // But different migration
        }

        let tempdir = tempfile::Builder::new()
            .prefix("DbTests")
            .tempdir()
            .unwrap();
        {
            // Setup the bad database
            let corrupt_db =
                open_db::<CorruptedDB>(tempdir.path(), release_channel::ReleaseChannel::Dev).await;
            assert!(corrupt_db.persistent());
        }

        // Try to connect to it a bunch of times at once
        let mut guards = vec![];
        for _ in 0..10 {
            let tmp_path = tempdir.path().to_path_buf();
            let guard = thread::spawn(move || {
                let good_db = gpui::block_on(open_db::<GoodDB>(
                    tmp_path.as_path(),
                    release_channel::ReleaseChannel::Dev,
                ));
                assert!(
                    good_db.select_row::<usize>("SELECT * FROM test2").unwrap()()
                        .unwrap()
                        .is_none()
                );
            });

            guards.push(guard);
        }

        for guard in guards.into_iter() {
            assert!(guard.join().is_ok());
        }
    }
}
