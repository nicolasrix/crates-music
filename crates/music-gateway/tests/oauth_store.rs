//! Gateway state DB: schema migration + OAuth client CRUD.
//!
//! Other tables (users, auth_codes, refresh_tokens, access_tokens) get their
//! tests in the sub-phases that introduce their accessors.

use music_gateway::oauth::{NewClient, OauthStore};

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
