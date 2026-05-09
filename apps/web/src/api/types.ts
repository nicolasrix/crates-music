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
  coverArt?: string;
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
}
