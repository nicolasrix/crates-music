//! Gateway state DB: schema migration + OAuth client CRUD.
//!
//! Other tables (users, auth_codes, refresh_tokens, access_tokens) get their
//! tests in the sub-phases that introduce their accessors.

use music_gateway::oauth::{NewClient, NewRefreshToken, NewUser, OauthStore};

#[tokio::test]
async fn open_in_memory_creates_all_oauth_tables() {
    let store = OauthStore::open_in_memory().await.unwrap();
    let names = store.table_names().await.unwrap();
    for expected in [
        "users",
        "oauth_clients",
        "auth_codes",
        "refresh_tokens",
        "access_tokens",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing table {expected}; got {names:?}"
        );
    }
}

#[tokio::test]
async fn register_and_lookup_oauth_client() {
    let store = OauthStore::open_in_memory().await.unwrap();
    let registered = store
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web App".to_string(),
            redirect_uris: vec!["http://localhost:5173/callback".to_string()],
        })
        .await
        .unwrap();
    assert_eq!(registered.client_id, "web");
    assert_eq!(registered.name, "Web App");

    let fetched = store.find_client("web").await.unwrap().unwrap();
    assert_eq!(fetched.client_id, "web");
    assert_eq!(
        fetched.redirect_uris,
        vec!["http://localhost:5173/callback".to_string()]
    );
}

#[tokio::test]
async fn find_client_returns_none_for_unknown_id() {
    let store = OauthStore::open_in_memory().await.unwrap();
    assert!(store.find_client("nonexistent").await.unwrap().is_none());
}

#[tokio::test]
async fn register_duplicate_client_id_fails() {
    let store = OauthStore::open_in_memory().await.unwrap();
    let new = NewClient {
        client_id: "web".to_string(),
        name: "Web App".to_string(),
        redirect_uris: vec!["http://x".to_string()],
    };
    store.register_client(new.clone()).await.unwrap();
    assert!(
        store.register_client(new).await.is_err(),
        "duplicate client_id must be rejected by the PRIMARY KEY constraint"
    );
}

#[tokio::test]
async fn redirect_uris_round_trip_multiple_entries() {
    let store = OauthStore::open_in_memory().await.unwrap();
    let uris = vec![
        "https://gateway.local:8443/web/callback".to_string(),
        "http://localhost:5173/callback".to_string(),
    ];
    store
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: uris.clone(),
        })
        .await
        .unwrap();
    let fetched = store.find_client("web").await.unwrap().unwrap();
    assert_eq!(fetched.redirect_uris, uris);
}

#[tokio::test]
async fn file_backed_store_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    {
        let store = OauthStore::open(&path).await.unwrap();
        store
            .register_client(NewClient {
                client_id: "cli".to_string(),
                name: "CLI".to_string(),
                redirect_uris: vec![],
            })
            .await
            .unwrap();
    }
    let store = OauthStore::open(&path).await.unwrap();
    let fetched = store.find_client("cli").await.unwrap().unwrap();
    assert_eq!(fetched.name, "CLI");
    assert!(fetched.redirect_uris.is_empty());
}

#[tokio::test]
async fn opening_same_file_twice_is_safe() {
    // Migrations run on every open; running them twice must be a no-op.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let _a = OauthStore::open(&path).await.unwrap();
    let _b = OauthStore::open(&path).await.unwrap();
}

// ---------------------------------------------------------------------
// User management (PR B)
// ---------------------------------------------------------------------

async fn seeded() -> OauthStore {
    let store = OauthStore::open_in_memory().await.unwrap();
    store
        .set_master_password_hash("$argon2id$dummy")
        .await
        .unwrap();
    store
}

#[tokio::test]
async fn find_login_user_defaults_to_owner_when_username_absent() {
    let store = seeded().await;
    // No username → owner (id=1), so the bootstrap master-password login
    // keeps working with nothing typed.
    let owner = store.find_login_user(None).await.unwrap().unwrap();
    assert_eq!(owner.id, 1);
    assert_eq!(owner.role, "admin");
    assert!(owner.password_hash.is_some());
    // Empty string behaves the same as absent.
    let owner2 = store.find_login_user(Some("")).await.unwrap().unwrap();
    assert_eq!(owner2.id, 1);
}

#[tokio::test]
async fn find_login_user_resolves_a_real_account_and_misses_unknown() {
    let store = seeded().await;
    let id = store
        .create_account(NewUser {
            username: Some("alice".to_string()),
            display_name: Some("Alice".to_string()),
            role: "user".to_string(),
            password_hash: Some("$argon2id$alice".to_string()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();
    let alice = store.find_login_user(Some("alice")).await.unwrap().unwrap();
    assert_eq!(alice.id, id);
    assert_eq!(alice.role, "user");
    assert!(store.find_login_user(Some("nobody")).await.unwrap().is_none());
}

#[tokio::test]
async fn create_account_rejects_duplicate_username() {
    let store = seeded().await;
    let mk = || NewUser {
        username: Some("dup".to_string()),
        display_name: None,
        role: "user".to_string(),
        password_hash: Some("$argon2id$x".to_string()),
        host_user_id: None,
        expires_at_unix_ms: None,
    };
    store.create_account(mk()).await.unwrap();
    let err = store.create_account(mk()).await.unwrap_err();
    assert!(
        matches!(err, music_gateway::oauth::Error::UsernameTaken),
        "second insert must surface UsernameTaken, got {err:?}"
    );
}

#[tokio::test]
async fn list_users_excludes_guests() {
    let store = seeded().await;
    store
        .create_account(NewUser {
            username: Some("alice".to_string()),
            display_name: None,
            role: "user".to_string(),
            password_hash: Some("$argon2id$x".to_string()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();
    // A guest row (as PR D would create) must not appear in the admin list.
    store
        .insert_user(NewUser {
            username: None,
            display_name: Some("Guest".to_string()),
            role: "guest".to_string(),
            password_hash: None,
            host_user_id: Some(1),
            expires_at_unix_ms: Some(now_ms() + 60_000),
        })
        .await
        .unwrap();
    let users = store.list_users().await.unwrap();
    assert!(users.iter().any(|u| u.username.as_deref() == Some("owner")));
    assert!(users.iter().any(|u| u.username.as_deref() == Some("alice")));
    assert!(users.iter().all(|u| u.role != "guest"), "guests excluded");
}

#[tokio::test]
async fn delete_user_cascades_tokens() {
    let store = seeded().await;
    // refresh_tokens.client_id FK-references oauth_clients; register it.
    store
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec![],
        })
        .await
        .unwrap();
    let id = store
        .create_account(NewUser {
            username: Some("alice".to_string()),
            display_name: None,
            role: "user".to_string(),
            password_hash: Some("$argon2id$x".to_string()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();
    let refresh = store
        .mint_refresh_token(NewRefreshToken {
            client_id: "web".to_string(),
            user_id: id,
            ttl: None,
        })
        .await
        .unwrap();
    assert!(store.find_refresh_token(&refresh.token).await.unwrap().is_some());

    assert!(store.delete_user(id).await.unwrap());
    // ON DELETE CASCADE drops the user's refresh token with them.
    assert!(
        store.find_refresh_token(&refresh.token).await.unwrap().is_none(),
        "deleting a user must cascade-delete their tokens"
    );
    // Deleting again is a no-op (already gone).
    assert!(!store.delete_user(id).await.unwrap());
}

#[tokio::test]
async fn set_user_password_rewrites_in_place_and_refuses_guests() {
    let store = seeded().await;
    let id = store
        .create_account(NewUser {
            username: Some("alice".to_string()),
            display_name: None,
            role: "user".to_string(),
            password_hash: Some("$argon2id$old".to_string()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();
    assert!(store.set_user_password(id, "$argon2id$new").await.unwrap());
    let alice = store.find_login_user(Some("alice")).await.unwrap().unwrap();
    assert_eq!(alice.password_hash.as_deref(), Some("$argon2id$new"));

    // A guest has no password to set.
    let guest = store
        .insert_user(NewUser {
            username: None,
            display_name: Some("Guest".to_string()),
            role: "guest".to_string(),
            password_hash: None,
            host_user_id: Some(1),
            expires_at_unix_ms: Some(now_ms() + 60_000),
        })
        .await
        .unwrap();
    assert!(!store.set_user_password(guest, "$argon2id$nope").await.unwrap());
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}
