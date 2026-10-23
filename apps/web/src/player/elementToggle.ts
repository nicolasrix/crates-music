// Whether a `play` / `pause` event from the <audio> element is news the
// room should hear — headphone keys, OS media controls, the tab-mute
// affordance — or only the echo of a play()/pause() we issued ourselves.
//
// Media events are queued tasks, so by the time one fires it may describe
// a transition that has already been undone: pause() then play() fires
// `pause` while the element is playing again. Judged against the *current*
// sync flag, that stale event looks like a divergence and gets submitted,
// which flips the flag, which calls play()/pause() again, which queues the
// next stale event — an oscillation at task rate (choppy audio, a flashing
// play button, ~800 ops/s to the gateway on 2026-10-23). It stayed latent
// while ops took effect only on the gateway echo; applying them locally
// moves the flag before the queued events fire.
//
// So the element's state at fire time decides, never the event's name.

export interface ElementState {
  paused: boolean;
  ended: boolean;
}

/** The `is_playing` to submit for this event, or null to ignore it. */
export function externalPlayingChange(el: ElementState, isPlaying: boolean): boolean | null {
  // A track ending pauses as a side effect; the auto-advance handler owns
  // that transition (it may flip is_playing at the queue tail).
  if (el.ended) return null;
  const playing = !el.paused;
  return playing === isPlaying ? null : playing;
}
