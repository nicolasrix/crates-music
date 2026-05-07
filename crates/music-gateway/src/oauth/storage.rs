//! Gateway state DB. Wraps a SQLite pool and runs migrations.
//!
//! At this stage the storage surface is minimal: schema bring-up plus
//! `oauth_clients` CRUD. User credentials, auth codes, and tokens get
//! their accessors added in the sub-phases that need them — keeping the
//! API close to its first caller is the cheap way to avoid a graveyard of
//! unused methods.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("malformed redirect_uris JSON in DB: {0}")]
    DecodeRedirectUris(serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Input shape for `register_client`. Created-at timestamp is filled in by
/// the store so callers can't forge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewClient {
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthClient {
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub created_at_unix_ms: i64,
}

#[derive(Debug, Clone)]
pub struct OauthStore {
    pool: SqlitePool,
}

impl OauthStore {
    /// In-memory store. Tests use this; production never should — it
    /// disappears with the process and would lose every refresh token.
    pub async fn open_in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            // In-memory SQLite is single-connection only — extra connections
            // each get their own private DB.
            .max_connections(1)
            .connect_with(opts)
            .await?;
        run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn register_client(&self, client: NewClient) -> Result<OauthClient> {
        let now = unix_ms_now();
        let uris_json = serde_json::to_string(&client.redirect_uris)
            .expect("Vec<String> always serializes to JSON");
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, name, redirect_uris, created_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&client.client_id)
        .bind(&client.name)
        .bind(&uris_json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(OauthClient {
            client_id: client.client_id,
            name: client.name,
            redirect_uris: client.redirect_uris,
            created_at_unix_ms: now,
        })
    }

    /// Fetch the master-password PHC string. `None` if the gateway has
    /// not been bootstrapped yet (no row in `users`).
    pub async fn master_password_hash(&self) -> Result<Option<String>> {
        let row = sqlx::query("SELECT password_hash FROM users WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("password_hash")))
    }

    /// Insert the master-password hash for the (single) user. Errors if a
    /// row already exists — changing the master password is a separate
    /// admin flow, not yet implemented.
    pub async fn set_master_password_hash(&self, phc: &str) -> Result<()> {
        let now = unix_ms_now();
        sqlx::query(
            "INSERT INTO users (id, password_hash, created_at) \
             VALUES (1, ?, ?)",
        )
        .bind(phc)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_client(&self, client_id: &str) -> Result<Option<OauthClient>> {
        let row = sqlx::query(
            "SELECT client_id, name, redirect_uris, created_at \
             FROM oauth_clients WHERE client_id = ?",
        )
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let uris_json: String = row.get("redirect_uris");
        let redirect_uris: Vec<String> =
            serde_json::from_str(&uris_json).map_err(Error::DecodeRedirectUris)?;
        Ok(Some(OauthClient {
            client_id: row.get("client_id"),
            name: row.get("name"),
            redirect_uris,
            created_at_unix_ms: row.get("created_at"),
        }))
    }

    /// Diagnostic: list every table in the DB. Used by tests to verify the
    /// migration applied; also useful for an admin/debug endpoint later.
    pub async fn table_names(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("name"))
            .collect())
    }
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

fn unix_ms_now() -> i64 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    i64::try_from(ms).unwrap_or(i64::MAX)
}
