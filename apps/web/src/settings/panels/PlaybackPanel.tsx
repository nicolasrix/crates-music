import { Volume2 } from "lucide-react";
import { useState } from "react";

import { usePlayer } from "../../player/PlayerContext";
import { SelectRow, SettingsSection, ToggleRow } from "../controls";
import { loadStreamQuality, saveStreamQuality, StreamQuality } from "../playback";
import { QUALITY_OPTIONS } from "../quality";

const ICON = { size: 18, strokeWidth: 1.5 } as const;

export function PlaybackPanel() {
  const [streamQuality, setStreamQuality] = useState<StreamQuality>(loadStreamQuality);
  const { outputEnabled, setOutputEnabled } = usePlayer();

  const update = (q: StreamQuality) => {
    setStreamQuality(q);
    saveStreamQuality(q);
  };

  return (
    <SettingsSection icon={<Volume2 {...ICON} />} title="playback">
      <SelectRow
        id="stream-quality"
        label="Streaming quality"
        value={streamQuality}
        options={QUALITY_OPTIONS}
        help="Transcode target for live playback of tracks that aren't cached. Lower it on a metered connection — opus 128 streams ~8× lighter than FLAC. Independent of offline-download quality; affects the next track loaded."
        onChange={update}
      />
      <ToggleRow
        label="Play audio on this device"
        checked={outputEnabled}
        help="On by default. Turn it off to use this device as a silent remote: it still controls the queue and play/pause, but audio plays on your other signed-in device. With two devices signed into the same account, leave both on to play in sync, or turn one off to make the other the speaker."
        onChange={setOutputEnabled}
      />
    </SettingsSection>
  );
}
