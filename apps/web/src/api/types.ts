// Subsonic / OpenSubsonic response shapes. Limited to what the UI
// actually consumes today — adding fields is cheap when needed.

export interface Album {
  id: string;
  name: string;
  artist?: string;
  artistId?: string;
  songCount?: number;
  duration?: number;
  year?: number;
  /** Album-level genre tag as reported by Subsonic. Single string (the
   *  first/primary genre); albums with no genre tag omit it. */
  genre?: string;
  coverArt?: string;
  /** Aggregate play count across the album's tracks. OpenSubsonic
   *  extension; older servers omit it. Source of truth lives upstream
   *  in Navidrome — refreshed when an /rest/getAlbum response lands. */
  playCount?: number;
  /** ISO8601 timestamp of the most recent play across the album.
   *  Format strictly as returned by the server; the UI parses with
   *  the platform Intl APIs. */
  played?: string;
}

export interface Track {
  id: string;
  title: string;
  album?: string;
  albumId?: string;
  artist?: string;
  artistId?: string;
  duration?: number;
  track?: number;
  coverArt?: string;
  /** Total times this track has been played. See Album.playCount for
   *  caveats on freshness. */
  playCount?: number;
  /** ISO8601 timestamp of the most recent play. */
  played?: string;
  /** ISO8601 timestamp of when the track was added to the library
   *  (Navidrome / OpenSubsonic `created` field on the song entity).
   *  Currently unused for sorting — "recently added" views derive
   *  their order from getAlbumList2?type=newest instead, which
   *  guarantees they match the recently-added-albums view. Kept on
   *  the type because the field is part of the wire format and may
   *  be useful for future filters. */
  created?: string;
}

export interface AlbumWithTracks {
  album: Album;
  tracks: Track[];
}

export interface Artist {
  id: string;
  name: string;
  albumCount?: number;
  coverArt?: string;
  artistImageUrl?: string;
}

export interface ArtistWithAlbums {
  artist: Artist;
  albums: Album[];
  biography?: string;
}

export interface PlaylistSummary {
  id: string;
  name: string;
  songCount?: number;
  duration?: number;
  coverArt?: string;
}

export interface PlaylistWithTracks {
  playlist: PlaylistSummary;
  tracks: Track[];
  /** The playlist's raw stored track ids, in order — the source of truth
   *  for membership edits (reorder / remove). `tracks` is the hydrated
   *  subset (ids that failed to resolve against the catalog are dropped),
   *  so edits must be computed against `trackIds`, never `tracks`, or a
   *  transient hydration miss would silently delete that id from storage. */
  trackIds: string[];
}
