// Durable like/dislike pill for a track, album, or artist — distinct from
// the recommendation thumbs (RecommendationFeedback). This rates *the entity
// itself*: a like boosts it (and its tracks) in recommendations and adds it
// to the Liked page; a dislike excludes it from play entirely (its tracks are
// dropped from recommendations and the player auto-skips them). The boost is
// weighted server-side track > album > artist. Heart / HeartCrack icons keep
// it visually separate from the ThumbsUp/ThumbsDown rec-feedback.

import { Heart, HeartCrack } from "lucide-react";
import { useEntityRating } from "../player/useRatings";
import type { EntityKind } from "../api/library";

// Per-kind microcopy. The noun drives the inline label + button tooltips so
// the control reads naturally wherever it's placed (player bar, album hero,
// artist hero).
const NOUN: Record<EntityKind, string> = {
  track: "song",
  album: "album",
  artist: "artist",
};

const LIKE_HINT: Record<EntityKind, string> = {
  track: "like — boosts recommendations and adds to Liked",
  album: "like — boosts this album's tracks in recommendations",
  artist: "like — boosts this artist's tracks in recommendations",
};

const DISLIKE_HINT: Record<EntityKind, string> = {
  track: "dislike — excluded from recommendations and auto-skipped",
  album: "dislike — excludes this album from play and recommendations",
  artist: "dislike — excludes this artist from play and recommendations",
};

export function EntityRating({
  kind,
  id,
}: {
  kind: EntityKind;
  id: string | undefined;
}) {
  const { rating, pending, set } = useEntityRating(kind, id);
  const noun = NOUN[kind];
  return (
    <div className="rec-feedback" role="group" aria-label={`like or dislike this ${noun}`}>
      <span className="rec-feedback__label" aria-hidden="true">
        rate {noun}
      </span>
      <button
        type="button"
        className={`rec-feedback__btn up ${rating === "like" ? "is-active" : ""}`}
        aria-label={`like this ${noun}`}
        aria-pressed={rating === "like"}
        title={LIKE_HINT[kind]}
        disabled={pending || !id}
        onClick={() => set("like")}
      >
        <Heart size={14} strokeWidth={1.75} fill={rating === "like" ? "currentColor" : "none"} />
      </button>
      <button
        type="button"
        className={`rec-feedback__btn down ${rating === "dislike" ? "is-active" : ""}`}
        aria-label={`dislike this ${noun}`}
        aria-pressed={rating === "dislike"}
        title={DISLIKE_HINT[kind]}
        disabled={pending || !id}
        onClick={() => set("dislike")}
      >
        <HeartCrack
          size={14}
          strokeWidth={1.75}
          fill={rating === "dislike" ? "currentColor" : "none"}
        />
      </button>
    </div>
  );
}
