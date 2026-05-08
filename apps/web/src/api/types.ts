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
  duration?: number;
  track?: number;
  coverArt?: string;
}

export interface AlbumWithTracks {
  album: Album;
  tracks: Track[];
}
