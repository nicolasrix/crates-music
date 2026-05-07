//! `play_queue_blocking` exercises the gapless queue path. The audio device
//! is not available in CI, so only the empty-queue degenerate is tested
//! here — actual gapless behaviour is verified manually.

use music_player::play_queue_blocking;

#[test]
fn empty_queue_is_no_op_and_does_not_open_audio_device() {
    // Must not call `OutputStream::try_default` for an empty queue — that
    // would fail in CI / on machines without a sound card. The function
    // should short-circuit before touching the audio stack.
    play_queue_blocking(vec![]).expect("empty queue should succeed cleanly");
}
