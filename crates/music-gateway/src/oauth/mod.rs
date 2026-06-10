//! OAuth 2.1 server: storage, password handling, endpoints.
//!
//! Single-user / multi-device. Each client (web app, CLI, phone) gets its
//! own refresh token so a stolen device is revoked individually. Storage
//! lives in its own SQLite file (separate from the L2 cache) because the
//! two have very different lifecycles — cache rows are throwaway, refresh
//! tokens are not.

pub mod handlers;
pub mod password;
pub mod session;
pub mod setup;
pub mod storage;

pub use session::{IssuedSession, Session};
pub use setup::SetupToken;
pub use storage::{
    AccessToken, AuthCode, DeviceCodeRow, DevicePollState, Error, GuestCodeRow, IssuedAccessToken,
    IssuedAuthCode, IssuedDeviceCode, IssuedGuestCode, IssuedRefreshToken, LoginUser, NewAuthCode,
    NewClient, NewDeviceCode, NewGuestCode, NewRefreshToken, NewUser, OauthClient, OauthStore,
    RedeemOutcome, RefreshToken, Result, UserSummary,
};
