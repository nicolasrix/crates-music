//! Opaque newtypes for catalog IDs.
//!
//! IDs are strings on the wire (Subsonic format), but we wrap them in
//! distinct types so a `TrackId` can never be accidentally passed where an
//! `AlbumId` is expected. Each type derives serde transparency so it
//! still serializes as a plain string.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl<S: Into<String>> From<S> for $name {
            fn from(s: S) -> Self {
                Self(s.into())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

id_type!(
    /// Identifier for a single track / song.
    TrackId
);
id_type!(
    /// Identifier for an album.
    AlbumId
);
id_type!(
    /// Identifier for an artist.
    ArtistId
);
id_type!(
    /// Identifier for a single item *position* in a queue.
    ///
    /// Distinct from [`TrackId`] because a track can legitimately appear
    /// in the queue more than once (intentional repeats), and reorder /
    /// remove ops need to address an item unambiguously even with
    /// duplicate `TrackId`s.
    QueueItemId
);
id_type!(
    /// Identifier for a recommend-session: one user-initiated playback
    /// "context" (the user picked a song or list to play; everything
    /// played from that anchor until the next direct play / explicit
    /// stop is the same session). Recommender uses this to scope
    /// downvotes/exclusions to a single listening session rather than
    /// the user's entire history. UUID v7 in practice.
    SessionId
);
