// Content-addressed key for a cached audio blob, mirroring the Rust
// `AudioKey` in crates/music-cache/src/audio.rs so the web cache and the
// CLI cache agree on identity (and a future shared core stays possible).
//
//   canonical form:  "{trackId}|{bitrate ?? 'orig'}|{codec}"
//
// For v1 the web client only ever stores the *passthrough original*
// variant, so `bitrate` is always null ("orig") and there is exactly one
// entry per track — but the full triple is kept so transcode-to-fit
// variants can be added later without a schema change.

export interface AudioKey {
  trackId: string;
  /** kbps, or null for the original/untranscoded stream. */
  bitrate: number | null;
  /** Short codec token, e.g. "mp3" | "flac" | "opus". */
  codec: string;
}

/** The canonical string form used as the IndexedDB primary key. */
export function canonicalKey(k: AudioKey): string {
  return `${k.trackId}|${k.bitrate ?? "orig"}|${k.codec}`;
}

const MIME_TO_CODEC: Record<string, string> = {
  "audio/mpeg": "mp3",
  "audio/mp3": "mp3",
  "audio/flac": "flac",
  "audio/x-flac": "flac",
  "audio/ogg": "ogg",
  "audio/opus": "opus",
  "audio/aac": "aac",
  "audio/mp4": "m4a",
  "audio/x-m4a": "m4a",
  "audio/wav": "wav",
  "audio/x-wav": "wav",
  "audio/webm": "webm",
};

/** Derive a short codec token from a response `Content-Type`. The web
 *  `Track` type carries no `suffix`, so the stream response is the only
 *  reliable signal for what Navidrome actually served. */
export function codecFromContentType(contentType: string | null | undefined): string {
  if (!contentType) return "bin";
  const mime = contentType.split(";")[0]!.trim().toLowerCase();
  if (MIME_TO_CODEC[mime]) return MIME_TO_CODEC[mime]!;
  return mime.replace(/^audio\//, "") || "bin";
}
