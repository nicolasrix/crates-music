//! Identity & authorization primitives.
//!
//! Every authenticated request resolves to a [`Principal`] in
//! `require_bearer`, which injects it as an `axum::Extension`. Handlers
//! that care about identity pull it back out with the [`AuthPrincipal`]
//! extractor; handlers that only need "any authenticated caller" ignore
//! it. Coarse route-level gating uses [`require_role`].
//!
//! This is the seam established by PR A of the user-system plan
//! (`docs/plans/user-system.md`). Today there is exactly one user (the
//! owner, an admin), so the capability checks never deny — but the wiring
//! is in place so PR B's real accounts and PR D's guests slot in without
//! touching every handler.

use axum::{
    Json,
    extract::{FromRequestParts, Request},
    http::{StatusCode, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::json;

/// The owner row seeded by migration `0004`. The static-bearer fallback
/// and any token with a NULL `user_id` (legacy rows backfilled by `0005`)
/// resolve to this id.
pub const OWNER_USER_ID: i64 = 1;

/// Who a request acts as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    User,
    Guest,
}

impl Role {
    /// Parse the `role` column (`'admin' | 'user' | 'guest'`). Unknown
    /// strings return `None` so a corrupt row fails closed rather than
    /// silently granting access.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "admin" => Some(Self::Admin),
            "user" => Some(Self::User),
            "guest" => Some(Self::Guest),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
            Self::Guest => "guest",
        }
    }

    /// Capability map. The single source of truth for "what may this role
    /// do" — see the roles→capabilities table in the plan doc. Read each
    /// arm as "this capability is granted to these roles":
    ///   * `AdminTools` — admin only (maintenance, diagnostics, provisioning).
    ///   * `WriteTaste` / `WritePlaylist` — real accounts (admin + user);
    ///     guests never reshape taste or own playlists.
    ///   * `ControlRoom` — everyone, including guests (the shared jukebox).
    #[must_use]
    pub fn can(self, cap: Capability) -> bool {
        match cap {
            Capability::AdminTools => matches!(self, Self::Admin),
            Capability::WriteTaste | Capability::WritePlaylist => {
                matches!(self, Self::Admin | Self::User)
            }
            Capability::ControlRoom => matches!(self, Self::Admin | Self::User | Self::Guest),
        }
    }
}

/// A discrete permission, decoupled from the route it guards so the same
/// check reads the same everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// `/v1/admin/*`, `/v1/diagnostics/*`, recommender maintenance
    /// (`refit_whitening`, `enqueue`), provisioning.
    AdminTools,
    /// Like/dislike + events that feed the recommender / play-counts.
    WriteTaste,
    /// Create/edit/delete playlists (gateway-owned; PR F).
    WritePlaylist,
    /// Add/reorder/skip in the room queue.
    ControlRoom,
}

/// The resolved identity of an authenticated request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Principal {
    pub user_id: i64,
    pub role: Role,
    /// Set only for guests — the User whose room they joined (PR D). For
    /// real accounts this is `None` and the room is their own id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_user_id: Option<i64>,
}

impl Principal {
    /// The owner/admin. Used by the static-bearer fallback and as the
    /// resolution for NULL-`user_id` legacy tokens.
    #[must_use]
    pub const fn owner() -> Self {
        Self {
            user_id: OWNER_USER_ID,
            role: Role::Admin,
            host_user_id: None,
        }
    }

    /// The sync partition this principal reads/writes. A User owns their
    /// own room (`user_id`); a guest attaches to their host's room.
    #[must_use]
    pub fn room_id(&self) -> i64 {
        self.host_user_id.unwrap_or(self.user_id)
    }

    #[must_use]
    pub fn can(&self, cap: Capability) -> bool {
        self.role.can(cap)
    }
}

/// Extractor that pulls the `Principal` injected by `require_bearer`.
///
/// A missing extension means the route was reached without passing the
/// bearer middleware — a router wiring bug, not a client error — so we
/// fail closed with 401.
#[derive(Debug)]
pub struct AuthPrincipal(pub Principal);

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for AuthPrincipal
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Principal>()
            .copied()
            .map(AuthPrincipal)
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

fn forbidden() -> impl IntoResponse {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "forbidden",
            "message": "your role does not permit this action",
        })),
    )
}

/// Admin-only route guard, used as an axum `from_fn` layer. Reads the
/// `Principal` injected upstream by `require_bearer` and 403s anyone
/// without `AdminTools`. A missing principal means the layer was wired
/// ahead of the bearer middleware — fail closed.
pub async fn require_admin(request: Request, next: Next) -> Response {
    match request.extensions().get::<Principal>().copied() {
        Some(principal) if principal.can(Capability::AdminTools) => next.run(request).await,
        Some(_) => forbidden().into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_map_matches_plan() {
        // Admin: everything.
        for cap in [
            Capability::AdminTools,
            Capability::WriteTaste,
            Capability::WritePlaylist,
            Capability::ControlRoom,
        ] {
            assert!(Role::Admin.can(cap), "admin should have {cap:?}");
        }
        // User: all but AdminTools.
        assert!(!Role::User.can(Capability::AdminTools));
        assert!(Role::User.can(Capability::WriteTaste));
        assert!(Role::User.can(Capability::WritePlaylist));
        assert!(Role::User.can(Capability::ControlRoom));
        // Guest: room control only.
        assert!(Role::Guest.can(Capability::ControlRoom));
        assert!(!Role::Guest.can(Capability::WriteTaste));
        assert!(!Role::Guest.can(Capability::WritePlaylist));
        assert!(!Role::Guest.can(Capability::AdminTools));
    }

    #[test]
    fn room_id_prefers_host() {
        let user = Principal {
            user_id: 7,
            role: Role::User,
            host_user_id: None,
        };
        assert_eq!(user.room_id(), 7);
        let guest = Principal {
            user_id: 42,
            role: Role::Guest,
            host_user_id: Some(7),
        };
        assert_eq!(guest.room_id(), 7);
    }

    #[test]
    fn role_db_roundtrip() {
        for role in [Role::Admin, Role::User, Role::Guest] {
            assert_eq!(Role::from_db_str(role.as_db_str()), Some(role));
        }
        assert_eq!(Role::from_db_str("nonsense"), None);
    }

    #[test]
    fn owner_is_admin() {
        let o = Principal::owner();
        assert_eq!(o.user_id, OWNER_USER_ID);
        assert_eq!(o.role, Role::Admin);
        assert_eq!(o.room_id(), OWNER_USER_ID);
    }
}
