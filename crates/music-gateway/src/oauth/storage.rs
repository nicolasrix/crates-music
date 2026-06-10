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
    /// The logged-in user the authorizing session belongs to. Carried
    /// onto the minted token pair so the access token resolves to the
    /// right principal (PR B).
    pub user_id: i64,
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
    pub user_id: i64,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRefreshToken {
    pub client_id: String,
    /// The owning user. Preserved across rotation so a refreshed pair
    /// keeps the same identity (PR B).
    pub user_id: i64,
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
    pub user_id: i64,
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

/// Input for `create_device_code`. Issued-at is filled in by the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDeviceCode {
    pub client_id: String,
    pub ttl: Duration,
    /// Polling interval handed back to the client (RFC 8628 §3.2).
    pub interval: Duration,
}

/// Plaintext-bearing return value from `create_device_code`. Both codes
/// are returned once: the `device_code` goes to the polling client (only
/// its hash is stored), the `user_code` is shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedDeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub expires_at_unix_ms: i64,
    pub interval_secs: i64,
}

/// A device-code row looked up by `user_code` (for the approval page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCodeRow {
    pub user_code: String,
    pub client_id: String,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
    pub approved_at_unix_ms: Option<i64>,
    pub denied_at_unix_ms: Option<i64>,
    pub consumed_at_unix_ms: Option<i64>,
}

/// Outcome of a token-endpoint poll against a device code (RFC 8628 §3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePollState {
    /// User hasn't decided yet → `authorization_pending`.
    Pending,
    /// User approved; tokens were just minted for `client_id` (the consume
    /// is atomic, so this is returned exactly once). `user_id` is the
    /// approving browser session's user — `None` for a pre-PR-B row whose
    /// approval predates identity threading (resolves to the owner).
    Approved {
        client_id: String,
        user_id: Option<i64>,
    },
    /// User denied → `access_denied`.
    Denied,
    /// Unknown, expired, or already-consumed code → `expired_token`.
    Expired,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("malformed redirect_uris JSON in DB: {0}")]
    DecodeRedirectUris(serde_json::Error),
    #[error("username already taken")]
    UsernameTaken,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Input for `insert_user`. `created_at` is stamped by the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUser {
    pub username: Option<String>,
    pub display_name: Option<String>,
    /// `'admin' | 'user' | 'guest'` — validated by the schema CHECK.
    pub role: String,
    pub password_hash: Option<String>,
    pub host_user_id: Option<i64>,
    pub expires_at_unix_ms: Option<i64>,
}

/// Credentials needed by the login flow: the row's id, its Argon2 hash
/// (absent for guests, who can't password-login), and its role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginUser {
    pub id: i64,
    pub password_hash: Option<String>,
    pub role: String,
}

/// Public-facing user row for the admin list. Deliberately omits
/// `password_hash` — the provisioning UI never sees credential material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSummary {
    pub id: i64,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub role: String,
    pub created_at_unix_ms: i64,
}

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

    /// Insert the owner row + master-password hash at bootstrap. Errors if a
    /// row already exists — rotating the master password is a separate admin
    /// flow (`set_user_password` on id=1).
    ///
    /// Seeds the owner as `id=1, username='owner', role='admin'` so the
    /// multi-user login path (which looks accounts up by username) can find
    /// it. A fresh `/oauth/setup` and an existing deployment migrated by
    /// `0004` therefore agree on the owner's identity shape.
    pub async fn set_master_password_hash(&self, phc: &str) -> Result<()> {
        let now = unix_ms_now();
        sqlx::query(
            "INSERT INTO users (id, username, display_name, role, password_hash, created_at) \
             VALUES (1, 'owner', 'Owner', 'admin', ?, ?)",
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

    /// Mint a new browser session bound to `user_id`, store its hash, and
    /// return the plaintext token (to put in `Set-Cookie`) plus its
    /// metadata. The `user_id` rides through every authorization this
    /// session grants (auth codes, device approvals) into the issued tokens.
    pub async fn create_session(&self, user_id: i64, ttl: Duration) -> Result<IssuedSession> {
        let token = session::mint_token();
        let token_hash = session::hash_token(&token);
        let issued_at = unix_ms_now();
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
        let expires_at = issued_at.saturating_add(ttl_ms);
        sqlx::query(
            "INSERT INTO sessions (token_hash, issued_at, expires_at, user_id) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&token_hash)
        .bind(issued_at)
        .bind(expires_at)
        .bind(user_id)
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
            "SELECT token_hash, user_id, issued_at, expires_at \
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
            // NULL only on a legacy/pre-PR-B row; treat as the owner.
            user_id: r.get::<Option<i64>, _>("user_id").unwrap_or(1),
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
                 code_challenge_method, issued_at, expires_at, user_id) \
             VALUES (?, ?, ?, ?, 'S256', ?, ?, ?)",
        )
        .bind(&code_hash)
        .bind(&input.client_id)
        .bind(&input.redirect_uri)
        .bind(&input.code_challenge)
        .bind(issued_at)
        .bind(expires_at)
        .bind(input.user_id)
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
                       issued_at, expires_at, user_id",
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
            user_id: r.get::<Option<i64>, _>("user_id").unwrap_or(1),
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
                (token_hash, client_id, issued_at, expires_at, user_id) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&token_hash)
        .bind(&input.client_id)
        .bind(issued_at)
        .bind(expires_at)
        .bind(input.user_id)
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
            "SELECT token_hash, client_id, user_id, issued_at, expires_at \
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
            user_id: r.get::<Option<i64>, _>("user_id").unwrap_or(1),
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
             RETURNING token_hash, client_id, user_id, issued_at, expires_at",
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
            user_id: r.get::<Option<i64>, _>("user_id").unwrap_or(1),
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

    /// Mint an access token with no explicit user attribution. The bearer
    /// middleware resolves a NULL `user_id` to the owner — see
    /// `resolve_principal`. PR B threads the real user via
    /// `mint_access_token_for_user`.
    pub async fn mint_access_token(
        &self,
        client_id: &str,
        refresh_token_hash: Option<&str>,
        ttl: Duration,
    ) -> Result<IssuedAccessToken> {
        self.mint_access_token_for_user(client_id, refresh_token_hash, ttl, None)
            .await
    }

    /// Mint an access token attributed to `user_id` (or unattributed when
    /// `None`). Once PR B's multi-user login lands, the token endpoint
    /// passes the logged-in user's id here.
    pub async fn mint_access_token_for_user(
        &self,
        client_id: &str,
        refresh_token_hash: Option<&str>,
        ttl: Duration,
        user_id: Option<i64>,
    ) -> Result<IssuedAccessToken> {
        let token = session::mint_token();
        let token_hash = session::hash_token(&token);
        let issued_at = unix_ms_now();
        let expires_at =
            issued_at.saturating_add(i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX));
        sqlx::query(
            "INSERT INTO access_tokens \
                (token_hash, client_id, refresh_token_hash, issued_at, expires_at, user_id) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&token_hash)
        .bind(client_id)
        .bind(refresh_token_hash)
        .bind(issued_at)
        .bind(expires_at)
        .bind(user_id)
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

    /// Insert a user row and return its assigned id. Roles are validated
    /// at the schema level (`CHECK (role IN ...)`). `password_hash` is the
    /// Argon2id PHC string for real accounts and `None` for guests;
    /// `host_user_id`/`expires_at` are guest-only.
    ///
    /// This is the low-level row insert; the admin-facing provisioning
    /// endpoint and guest-code redemption (PR B / PR D) call it after
    /// their own validation. Exposed in PR A so the authorization layer
    /// can be tested against real non-admin principals.
    pub async fn insert_user(&self, user: NewUser) -> Result<i64> {
        let now = unix_ms_now();
        let row = sqlx::query(
            "INSERT INTO users \
                (username, display_name, role, password_hash, host_user_id, expires_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             RETURNING id",
        )
        .bind(&user.username)
        .bind(&user.display_name)
        .bind(user.role)
        .bind(&user.password_hash)
        .bind(user.host_user_id)
        .bind(user.expires_at_unix_ms)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("id"))
    }

    /// Fetch a user's display fields for `whoami`. Returns
    /// `(username, display_name)` (both nullable — guests have neither).
    /// `None` if the id doesn't exist.
    pub async fn user_profile(
        &self,
        user_id: i64,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let row = sqlx::query("SELECT username, display_name FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| {
            (
                r.get::<Option<String>, _>("username"),
                r.get::<Option<String>, _>("display_name"),
            )
        }))
    }

    /// Insert a real account (admin/user), translating a username-uniqueness
    /// collision into the typed [`Error::UsernameTaken`] so the provisioning
    /// handler can map it to a 409 rather than a 500. Thin wrapper over
    /// [`insert_user`](Self::insert_user).
    pub async fn create_account(&self, user: NewUser) -> Result<i64> {
        match self.insert_user(user).await {
            Ok(id) => Ok(id),
            Err(Error::Sqlx(e)) if is_unique_violation(&e) => Err(Error::UsernameTaken),
            Err(e) => Err(e),
        }
    }

    /// Look up the credentials for a login attempt. `None`/empty username
    /// defaults to the owner (id=1), so the bootstrap "master password only"
    /// login keeps working with no username typed. A real username resolves
    /// that account. Returns `None` when no such row exists — the caller
    /// still burns a dummy verify so the timing doesn't leak existence.
    pub async fn find_login_user(&self, username: Option<&str>) -> Result<Option<LoginUser>> {
        let row = match username {
            Some(u) if !u.is_empty() => {
                sqlx::query("SELECT id, password_hash, role FROM users WHERE username = ?")
                    .bind(u)
                    .fetch_optional(&self.pool)
                    .await?
            }
            _ => {
                sqlx::query("SELECT id, password_hash, role FROM users WHERE id = 1")
                    .fetch_optional(&self.pool)
                    .await?
            }
        };
        Ok(row.map(|r| LoginUser {
            id: r.get("id"),
            password_hash: r.get::<Option<String>, _>("password_hash"),
            role: r.get("role"),
        }))
    }

    /// List the real accounts (admin + user), newest-row last. Guests are
    /// excluded — they're ephemeral and managed by the guest-code flow
    /// (PR D), not the user-provisioning UI. No credential material.
    pub async fn list_users(&self) -> Result<Vec<UserSummary>> {
        let rows = sqlx::query(
            "SELECT id, username, display_name, role, created_at \
             FROM users WHERE role IN ('admin', 'user') ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| UserSummary {
                id: r.get("id"),
                username: r.get::<Option<String>, _>("username"),
                display_name: r.get::<Option<String>, _>("display_name"),
                role: r.get("role"),
                created_at_unix_ms: r.get("created_at"),
            })
            .collect())
    }

    /// Delete a user by id and cascade their tokens/sessions (FK
    /// `ON DELETE CASCADE`, enabled per-connection by sqlx). Returns
    /// `true` if a row was removed. The caller must refuse to delete the
    /// owner (id=1) — this method does not, so it stays reusable for guest
    /// reaping (PR D).
    pub async fn delete_user(&self, id: i64) -> Result<bool> {
        let res = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Rewrite a real account's password hash in place. Used by the admin
    /// password-reset endpoint and the host-only `reset-master-password`
    /// subcommand (id=1). Returns `true` if a row was updated. Refuses
    /// guests (they have no password). The row is never deleted/recreated,
    /// so dependent data is preserved — see D8 in the plan.
    pub async fn set_user_password(&self, id: i64, phc: &str) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE users SET password_hash = ? \
             WHERE id = ? AND role IN ('admin', 'user')",
        )
        .bind(phc)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Resolve an active access token to its owning principal. Returns
    /// `(user_id, role_str, host_user_id)`.
    ///
    /// Identity rules (the `users` join is a LEFT JOIN so we can tell the
    /// two cases apart):
    ///   * **NULL `user_id`** — a legacy token (pre-multi-user) or a PR-A
    ///     token whose mint path doesn't set `user_id` yet. Resolves to the
    ///     owner/admin (id=1), mirroring the static-bearer fallback. We do
    ///     *not* require a `users` row for this case, so it works even on a
    ///     not-yet-bootstrapped store.
    ///   * **Explicit `user_id`** — must map to a live `users` row. A
    ///     missing row (deleted user) or an elapsed `expires_at` (lapsed
    ///     guest) yields `None`, so a token can't outlive its account even
    ///     while its own short TTL is unspent.
    ///
    /// Returns `None` for missing, expired, or revoked tokens — the
    /// identity-aware companion to `find_access_token`.
    pub async fn resolve_principal(
        &self,
        token: &str,
    ) -> Result<Option<(i64, String, Option<i64>)>> {
        let token_hash = session::hash_token(token);
        let now = unix_ms_now();
        let row = sqlx::query(
            "SELECT a.user_id AS raw_user_id, u.role AS role, \
                    u.host_user_id AS host_user_id, u.expires_at AS user_expires_at \
             FROM access_tokens a \
             LEFT JOIN users u ON u.id = a.user_id \
             WHERE a.token_hash = ? \
               AND a.revoked_at IS NULL \
               AND a.expires_at > ?",
        )
        .bind(&token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let raw_user_id: Option<i64> = row.get("raw_user_id");
        match raw_user_id {
            // Legacy / PR-A token → owner/admin, no users row required.
            None => Ok(Some((1, "admin".to_string(), None))),
            Some(user_id) => {
                // LEFT JOIN miss => user deleted out from under the token.
                let Some(role) = row.get::<Option<String>, _>("role") else {
                    return Ok(None);
                };
                let user_expires_at: Option<i64> = row.get("user_expires_at");
                if user_expires_at.is_some_and(|e| e <= now) {
                    return Ok(None); // lapsed guest account
                }
                let host_user_id: Option<i64> = row.get("host_user_id");
                Ok(Some((user_id, role, host_user_id)))
            }
        }
    }

    // -----------------------------------------------------------------
    // Device Authorization Grant (RFC 8628)
    // -----------------------------------------------------------------

    /// Mint and store a fresh device code + user code. The plaintext
    /// `device_code` is in the return value (goes to the polling client);
    /// the row holds only its sha256. `user_code` is stored in plaintext
    /// for the approval-page lookup.
    pub async fn create_device_code(&self, input: NewDeviceCode) -> Result<IssuedDeviceCode> {
        let device_code = session::mint_token();
        let device_code_hash = session::hash_token(&device_code);
        let user_code = session::mint_user_code();
        let issued_at = unix_ms_now();
        let ttl_ms = i64::try_from(input.ttl.as_millis()).unwrap_or(i64::MAX);
        let expires_at = issued_at.saturating_add(ttl_ms);
        let interval_secs = i64::try_from(input.interval.as_secs()).unwrap_or(5).max(1);
        sqlx::query(
            "INSERT INTO device_codes \
                (device_code_hash, user_code, client_id, issued_at, expires_at, interval_secs) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&device_code_hash)
        .bind(&user_code)
        .bind(&input.client_id)
        .bind(issued_at)
        .bind(expires_at)
        .bind(interval_secs)
        .execute(&self.pool)
        .await?;
        Ok(IssuedDeviceCode {
            device_code,
            user_code,
            expires_at_unix_ms: expires_at,
            interval_secs,
        })
    }

    /// Look up a device code by its (plaintext, user-typed) `user_code`,
    /// for the browser approval page. Returns `None` for an unknown code.
    pub async fn find_device_by_user_code(&self, user_code: &str) -> Result<Option<DeviceCodeRow>> {
        let row = sqlx::query(
            "SELECT user_code, client_id, issued_at, expires_at, \
                    approved_at, denied_at, consumed_at \
             FROM device_codes WHERE user_code = ?",
        )
        .bind(user_code)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| DeviceCodeRow {
            user_code: r.get("user_code"),
            client_id: r.get("client_id"),
            issued_at_unix_ms: r.get("issued_at"),
            expires_at_unix_ms: r.get("expires_at"),
            approved_at_unix_ms: r.get("approved_at"),
            denied_at_unix_ms: r.get("denied_at"),
            consumed_at_unix_ms: r.get("consumed_at"),
        }))
    }

    /// Record the user's approve/deny decision for a `user_code`. Only
    /// affects a row that is still pending (no decision, not consumed, not
    /// expired). Returns `true` if a decision was recorded, `false` if the
    /// code was unknown, already decided, consumed, or expired.
    ///
    /// On approval the approving browser session's `approver_user_id` is
    /// stamped onto the row so the device's minted tokens carry the real
    /// identity (PR B). Denials leave `user_id` untouched.
    pub async fn set_device_decision(
        &self,
        user_code: &str,
        approve: bool,
        approver_user_id: Option<i64>,
    ) -> Result<bool> {
        let now = unix_ms_now();
        // Approval additionally records who approved; denial only stamps the
        // timestamp. Two static SQL strings keep binding order unambiguous.
        let result = if approve {
            sqlx::query(
                "UPDATE device_codes SET approved_at = ?, user_id = ? \
                 WHERE user_code = ? \
                   AND approved_at IS NULL \
                   AND denied_at IS NULL \
                   AND consumed_at IS NULL \
                   AND expires_at > ?",
            )
            .bind(now)
            .bind(approver_user_id)
            .bind(user_code)
            .bind(now)
            .execute(&self.pool)
            .await?
        } else {
            sqlx::query(
                "UPDATE device_codes SET denied_at = ? \
                 WHERE user_code = ? \
                   AND approved_at IS NULL \
                   AND denied_at IS NULL \
                   AND consumed_at IS NULL \
                   AND expires_at > ?",
            )
            .bind(now)
            .bind(user_code)
            .bind(now)
            .execute(&self.pool)
            .await?
        };
        Ok(result.rows_affected() > 0)
    }

    /// Poll a device code from the token endpoint. Classifies the row, and
    /// — only when it's approved-and-unspent — atomically stamps
    /// `consumed_at` so tokens mint exactly once (the `UPDATE … RETURNING`
    /// is the concurrency gate, mirroring `consume_auth_code`). Unknown,
    /// expired, and already-consumed codes all collapse to `Expired`.
    pub async fn consume_device_code(&self, device_code: &str) -> Result<DevicePollState> {
        let device_code_hash = session::hash_token(device_code);
        let now = unix_ms_now();
        let row = sqlx::query(
            "SELECT expires_at, approved_at, denied_at, consumed_at \
             FROM device_codes WHERE device_code_hash = ?",
        )
        .bind(&device_code_hash)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(DevicePollState::Expired);
        };
        let expires_at: i64 = row.get("expires_at");
        let approved_at: Option<i64> = row.get("approved_at");
        let denied_at: Option<i64> = row.get("denied_at");
        let consumed_at: Option<i64> = row.get("consumed_at");

        if consumed_at.is_some() || expires_at <= now {
            return Ok(DevicePollState::Expired);
        }
        if denied_at.is_some() {
            return Ok(DevicePollState::Denied);
        }
        if approved_at.is_none() {
            return Ok(DevicePollState::Pending);
        }

        // Approved and unspent: atomically claim it. The WHERE re-checks
        // every precondition, so a concurrent poll can't double-mint — the
        // loser matches no row and falls through to `Expired`.
        let claimed = sqlx::query(
            "UPDATE device_codes SET consumed_at = ? \
             WHERE device_code_hash = ? \
               AND approved_at IS NOT NULL \
               AND denied_at IS NULL \
               AND consumed_at IS NULL \
               AND expires_at > ? \
             RETURNING client_id, user_id",
        )
        .bind(now)
        .bind(&device_code_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        match claimed {
            Some(r) => Ok(DevicePollState::Approved {
                client_id: r.get("client_id"),
                user_id: r.get::<Option<i64>, _>("user_id"),
            }),
            None => Ok(DevicePollState::Expired),
        }
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

/// Whether a sqlx error is a SQLite UNIQUE-constraint violation — used to
/// translate a duplicate `users.username` into [`Error::UsernameTaken`].
fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
}

fn unix_ms_now() -> i64 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    i64::try_from(ms).unwrap_or(i64::MAX)
}
