// Fisher-Yates shuffle. Returns a NEW array (the codebase's immutability
// rule — never reorder the caller's list in place, which would scramble the
// React key order of a rendered tracklist under the caller's feet).

/** An unbiased shuffle of `input`, as a fresh array. Empty and single-element
 *  inputs are returned as plain copies. */
export function shuffle<T>(input: readonly T[]): T[] {
  const out = [...input];
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    const tmp = out[i]!;
    out[i] = out[j]!;
    out[j] = tmp;
  }
  return out;
}
