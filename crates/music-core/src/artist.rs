use serde::{Deserialize, Serialize};

use crate::ids::ArtistId;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artist {
    pub id: ArtistId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_count: Option<u32>,
}
