// Hook backing the player's thumb-up / thumb-down widget.
//
// State machine for a single track:
//   - `vote`: what we believe the user has voted in this session.
//   - `cast(next)`: optimistically apply `next`, POST to gateway,
//     reconcile against the server-returned totals.
//
// Per-session memory lives in sessionStorage, keyed by track_id. That
// gives the user persistent visual confirmation of "yes, you already
// voted on this" if the track plays a second time in the same session,
// without polluting localStorage with what's effectively ephemeral.
//
// Errors are swallowed (best-effort signal capture). The next user
// gesture will re-emit; we don't want a transient 500 to lock the
// widget out of further votes.

import { useCallback, useEffect, useState } from "react";
import { submitFeedback, type FeedbackVote } from "../api/recommend";
import { getSessionId } from "../rum/session";
import { useSync } from "../sync/SyncContext";

const KEY_PREFIX = "crates-music.feedback.";

function loadCached(trackId: string): FeedbackVote {
  try {
    const v = sessionStorage.getItem(KEY_PREFIX + trackId);
    return v === "up" || v === "down" ? v : null;
  } catch {
    return null;
  }
}

function persistCached(trackId: string, vote: FeedbackVote) {
  try {
    if (vote === null) sessionStorage.removeItem(KEY_PREFIX + trackId);
    else sessionStorage.setItem(KEY_PREFIX + trackId, vote);
  } catch {
    /* private mode — fine, we just lose visual memo across re-mounts */
  }
}

export interface FeedbackHookState {
  vote: FeedbackVote;
  /** True while a POST is in flight. The buttons disable so a
   *  double-click doesn't double-fire. */
  pending: boolean;
  /** Cast a vote. Passing the *current* vote toggles it off (the
   *  user clicks an already-active thumb to undo). */
  cast: (next: "up" | "down") => void;
}

export function useRecommendationFeedback(
  trackId: string | undefined,
): FeedbackHookState {
  const { state } = useSync();
  const [vote, setVote] = useState<FeedbackVote>(null);
  const [pending, setPending] = useState(false);

  // Hydrate from sessionStorage when the track changes. Without this
  // the widget would always render as "no vote yet" even when the user
  // had voted earlier in the same page-load.
  useEffect(() => {
    if (!trackId) {
      setVote(null);
      return;
    }
    setVote(loadCached(trackId));
  }, [trackId]);

  const cast = useCallback(
    (next: "up" | "down") => {
      if (!trackId || pending) return;
      // Toggle-off: clicking the already-active thumb clears the vote.
      const target: FeedbackVote = vote === next ? null : next;
      // Optimistic update — the widget should feel instant.
      setVote(target);
      persistCached(trackId, target);
      setPending(true);
      // Prefer the recommend-session id so the gateway can scope the
      // downvote-exclusion to the user's current listening context.
      // Fall back to the RUM session id when no session is active —
      // the vote is still durably recorded for diagnostics either way.
      const sessionId =
        state.playback.session_anchor?.session_id ?? getSessionId();
      void submitFeedback({
        trackId,
        vote: target,
        sessionId,
      })
        .catch(() => {
          // Roll back the optimistic update on failure so the UI
          // matches what the server actually persisted.
          setVote(vote);
          persistCached(trackId, vote);
        })
        .finally(() => setPending(false));
    },
    [trackId, vote, pending, state.playback.session_anchor?.session_id],
  );

  return { vote, pending, cast };
}
