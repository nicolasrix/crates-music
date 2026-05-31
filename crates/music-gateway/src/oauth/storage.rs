//! Gateway state DB. Wraps a SQLite pool and runs migrations.
//!
//! At this stage the storage surface is minimal: schema bring-up plus
//! `oauth_clients` CRUD. User credentials, auth codes, and tokens get
//! their accessors added in the sub-phases that need them — keeping the
//! API close to its first caller is the cheap way to avoid a graveyard of
//! unused methods.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::oauth::session::{self, IssuedSession, Session};

/// Input for `create_auth_code`. Issued-at is filled in by the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuthCode {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub ttl: Duration,
}

/// Plaintext-bearing return value from `create_auth_code`. The plaintext
/// goes into the redirect URL once; the row only ever holds its hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedAuthCode {
    pub code: String,
    pub code_hash: String,
    pub expires_at_unix_ms: i64,
}

/// A consumed authorization code row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCode {
    pub code_hash: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRefreshToken {
    pub client_id: String,
    /// `None` = no expiry; rotation is the revocation path.
    pub ttl: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedRefreshToken {
    pub token: String,
    pub token_hash: String,
    pub expires_at_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshToken {
    pub token_hash: String,
    pub client_id: String,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedAccessToken {
    pub token: String,
    pub token_hash: String,
    pub expires_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessToken {
    pub token_hash: String,
    pub client_id: String,
    pub refresh_token_hash: Option<String>,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
}

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

    /// Mint a new browser session, store its hash, return the plaintext
    /// token (to put in `Set-Cookie`) plus its metadata.
    pub async fn create_session(&self, ttl: Duration) -> Result<IssuedSession> {
        let token = session::mint_token();
        let token_hash = session::hash_token(&token);
        let issued_at = unix_ms_now();
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
        let expires_at = issued_at.saturating_add(ttl_ms);
        sqlx::query(
            "INSERT INTO sessions (token_hash, issued_at, expires_at) \
             VALUES (?, ?, ?)",
        )
        .bind(&token_hash)
        .bind(issued_at)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(IssuedSession {
            token,
            token_hash,
            issued_at_unix_ms: issued_at,
            expires_at_unix_ms: expires_at,
        })
    }

    /// Look up a session by its plaintext cookie token. Returns `None`
    /// for unknown / expired / revoked sessions — callers don't need to
    /// distinguish.
    pub async fn find_session(&self, token: &str) -> Result<Option<Session>> {
        let token_hash = session::hash_token(token);
        let now = unix_ms_now();
        let row = sqlx::query(
            "SELECT token_hash, issued_at, expires_at \
             FROM sessions \
             WHERE token_hash = ? \
               AND expires_at > ? \
               AND revoked_at IS NULL",
        )
        .bind(&token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Session {
            token_hash: r.get("token_hash"),
            issued_at_unix_ms: r.get("issued_at"),
            expires_at_unix_ms: r.get("expires_at"),
        }))
    }

    /// Revoke a session by its plaintext token. Idempotent: a missing
    /// session is not an error (e.g. user clicks "log out" twice).
    pub async fn revoke_session(&self, token: &str) -> Result<()> {
        let token_hash = session::hash_token(token);
        let now = unix_ms_now();
        sqlx::query(
            "UPDATE sessions SET revoked_at = ? \
             WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(&token_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mint and store a fresh authorization code. The plaintext is in
    /// the return value (goes into the redirect URL); the row holds only
    /// its sha256.
    pub async fn create_auth_code(&self, input: NewAuthCode) -> Result<IssuedAuthCode> {
        let code = session::mint_token();
        let code_hash = session::hash_token(&code);
        let issued_at = unix_ms_now();
        let ttl_ms = i64::try_from(input.ttl.as_millis()).unwrap_or(i64::MAX);
        let expires_at = issued_at.saturating_add(ttl_ms);
        sqlx::query(
            "INSERT INTO auth_codes \
                (code_hash, client_id, redirect_uri, code_challenge, \
                 code_challenge_method, issued_at, expires_at) \
             VALUES (?, ?, ?, ?, 'S256', ?, ?)",
        )
        .bind(&code_hash)
        .bind(&input.client_id)
        .bind(&input.redirect_uri)
        .bind(&input.code_challenge)
        .bind(issued_at)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(IssuedAuthCode {
            code,
            code_hash,
            expires_at_unix_ms: expires_at,
        })
    }

    /// Single-use redemption: returns `Some(row)` exactly once, then
    /// `None` for every subsequent call (replay defense). Expired codes
    /// also return `None`.
    pub async fn consume_auth_code(&self, code: &str) -> Result<Option<AuthCode>> {
        let code_hash = session::hash_token(code);
        let now = unix_ms_now();
        // Atomic: UPDATE only if not yet consumed and not expired, return
        // the columns we'd then want to read. SQLite supports RETURNING
        // since 3.35.
        let row = sqlx::query(
            "UPDATE auth_codes \
             SET consumed_at = ? \
             WHERE code_hash = ? \
               AND consumed_at IS NULL \
               AND expires_at > ? \
             RETURNING code_hash, client_id, redirect_uri, code_challenge, \
                       issued_at, expires_at",
        )
        .bind(now)
        .bind(&code_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| AuthCode {
            code_hash: r.get("code_hash"),
            client_id: r.get("client_id"),
            redirect_uri: r.get("redirect_uri"),
            code_challenge: r.get("code_challenge"),
            issued_at_unix_ms: r.get("issued_at"),
            expires_at_unix_ms: r.get("expires_at"),
        }))
    }

    pub async fn mint_refresh_token(&self, input: NewRefreshToken) -> Result<IssuedRefreshToken> {
        let token = session::mint_token();
        let token_hash = session::hash_token(&token);
        let issued_at = unix_ms_now();
        let expires_at = input
            .ttl
            .map(|t| issued_at.saturating_add(i64::try_from(t.as_millis()).unwrap_or(i64::MAX)));
        sqlx::query(
            "INSERT INTO refresh_tokens \
                (token_hash, client_id, issued_at, expires_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&token_hash)
        .bind(&input.client_id)
        .bind(issued_at)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(IssuedRefreshToken {
            token,
            token_hash,
            expires_at_unix_ms: expires_at,
        })
    }

    /// Look up an active refresh token. Returns `None` for missing,
    /// expired, or revoked tokens.
    pub async fn find_refresh_token(&self, token: &str) -> Result<Option<RefreshToken>> {
        let token_hash = session::hash_token(token);
        let now = unix_ms_now();
        let row = sqlx::query(
            "SELECT token_hash, client_id, issued_at, expires_at \
             FROM refresh_tokens \
             WHERE token_hash = ? \
               AND revoked_at IS NULL \
               AND (expires_at IS NULL OR expires_at > ?)",
        )
        .bind(&token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| RefreshToken {
            token_hash: r.get("token_hash"),
            client_id: r.get("client_id"),
            issued_at_unix_ms: r.get("issued_at"),
            expires_at_unix_ms: r.get("expires_at"),
        }))
    }

    /// Atomic single-use redemption for rotation: revoke the refresh
    /// token (and every access token derived from it) and return its row
    /// **exactly once**, scoped to `client_id`. Returns `None` for a
    /// token that is missing, expired, already revoked, or registered to
    /// a different client.
    ///
    /// The single-row `UPDATE ... RETURNING` is the concurrency gate
    /// (mirrors `consume_auth_code`): two simultaneous refreshes of the
    /// same token can't both win because only the row whose `revoked_at`
    /// was still NULL is updated and returned — the loser matches no rows
    /// and gets `None`. SQLite serialises the write transactions, so the
    /// access-token cascade in the winning transaction is consistent.
    /// A non-matching `client_id` leaves the token untouched (the WHERE
    /// doesn't match), so a wrong-client attempt can't burn a valid token.
    pub async fn consume_refresh_token(
        &self,
        token: &str,
        client_id: &str,
    ) -> Result<Option<RefreshToken>> {
        let token_hash = session::hash_token(token);
        let now = unix_ms_now();
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "UPDATE refresh_tokens SET revoked_at = ? \
             WHERE token_hash = ? \
               AND client_id = ? \
               AND revoked_at IS NULL \
               AND (expires_at IS NULL OR expires_at > ?) \
             RETURNING token_hash, client_id, issued_at, expires_at",
        )
        .bind(now)
        .bind(&token_hash)
        .bind(client_id)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;

        // Cascade-revoke derived access tokens only when we actually
        // consumed the refresh (otherwise a no-op).
        if row.is_some() {
            sqlx::query(
                "UPDATE access_tokens SET revoked_at = ? \
                 WHERE refresh_token_hash = ? AND revoked_at IS NULL",
            )
            .bind(now)
            .bind(&token_hash)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        Ok(row.map(|r| RefreshToken {
            token_hash: r.get("token_hash"),
            client_id: r.get("client_id"),
            issued_at_unix_ms: r.get("issued_at"),
            expires_at_unix_ms: r.get("expires_at"),
        }))
    }

    /// Revoke a refresh token plus every access token derived from it.
    /// Idempotent on missing input.
    pub async fn revoke_refresh_token(&self, token: &str) -> Result<()> {
        let token_hash = session::hash_token(token);
        let now = unix_ms_now();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE refresh_tokens SET revoked_at = ? \
             WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(&token_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE access_tokens SET revoked_at = ? \
             WHERE refresh_token_hash = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(&token_hash)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn mint_access_token(
        &self,
        client_id: &str,
        refresh_token_hash: Option<&str>,
        ttl: Duration,
    ) -> Result<IssuedAccessToken> {
        let token = session::mint_token();
        let token_hash = session::hash_token(&token);
        let issued_at = unix_ms_now();
        let expires_at =
            issued_at.saturating_add(i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX));
        sqlx::query(
            "INSERT INTO access_tokens \
                (token_hash, client_id, refresh_token_hash, issued_at, expires_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&token_hash)
        .bind(client_id)
        .bind(refresh_token_hash)
        .bind(issued_at)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(IssuedAccessToken {
            token,
            token_hash,
            expires_at_unix_ms: expires_at,
        })
    }

    /// Revoke an access token by its plaintext value. Idempotent.
    pub async fn revoke_access_token(&self, token: &str) -> Result<()> {
        let token_hash = session::hash_token(token);
        let now = unix_ms_now();
        sqlx::query(
            "UPDATE access_tokens SET revoked_at = ? \
             WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(&token_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Look up an active access token. Returns `None` for missing,
    /// expired, or revoked tokens. Used by the bearer-token middleware.
    pub async fn find_access_token(&self, token: &str) -> Result<Option<AccessToken>> {
        let token_hash = session::hash_token(token);
        let now = unix_ms_now();
        let row = sqlx::query(
            "SELECT token_hash, client_id, refresh_token_hash, issued_at, expires_at \
             FROM access_tokens \
             WHERE token_hash = ? \
               AND revoked_at IS NULL \
               AND expires_at > ?",
        )
        .bind(&token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| AccessToken {
            token_hash: r.get("token_hash"),
            client_id: r.get("client_id"),
            refresh_token_hash: r.get("refresh_token_hash"),
            issued_at_unix_ms: r.get("issued_at"),
            expires_at_unix_ms: r.get("expires_at"),
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
