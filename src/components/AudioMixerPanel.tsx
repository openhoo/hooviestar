import { memo } from "react";
import { SpeakerIcon } from "./icons";

type LevelEntry = { sourceId: string; peak: number; rms: number };

interface AudioMixerPanelProps {
  channels: Array<{ sourceId: string; name: string; volume: number; muted: boolean }>;
  levels: LevelEntry[];
  audioError: string | null;
  onVolume: (sourceId: string, volume: number) => void;
  onToggleMute: (sourceId: string) => void;
}

function levelWidth(levels: LevelEntry[], sourceId: string): number {
  const peak = levels.find((entry) => entry.sourceId === sourceId)?.peak ?? 0;
  return Math.min(100, Math.max(0, Math.round(peak * 100)));
}

function toDb(volume: number): string {
  return volume <= 0 ? "−∞ dB" : `${(20 * Math.log10(volume)).toFixed(1)} dB`;
}

function AudioMixerPanelImpl({ channels, levels, audioError, onVolume, onToggleMute }: AudioMixerPanelProps) {
  return (
    <section className="dock mixer-dock" aria-label="Audio-Mixer">
      <div className="dock-title">
        <h2>Audio-Mixer</h2>
        <span>48 kHz · Stereo</span>
      </div>
      <div className="mixer-channels">
        {channels.map((channel) => (
          <div className="mixer-channel" key={channel.sourceId}>
            <div className="channel-head">
              <span className="channel-name" title={channel.name}>{channel.name}</span>
              <output className="db-readout">{toDb(channel.volume)}</output>
            </div>
            <div className="meter horizontal" aria-label={`Pegel ${channel.name}`}>
              <i style={{ width: `${levelWidth(levels, channel.sourceId)}%` }} />
            </div>
            <div className="db-scale" aria-hidden="true">
              <span>-60</span>
              <span>-40</span>
              <span>-20</span>
              <span>-12</span>
              <span>-6</span>
              <span>0</span>
            </div>
            <div className="fader-row">
              <input
                className="fader"
                type="range"
                min={0}
                max={1}
                step={0.01}
                value={channel.volume}
                aria-label={`Lautstärke ${channel.name}`}
                onChange={(event) => onVolume(channel.sourceId, Number(event.currentTarget.value))}
              />
              <button
                type="button"
                className={channel.muted ? "mute-button muted" : "mute-button"}
                aria-label={channel.muted ? "Ton einschalten" : "Stumm schalten"}
                title={channel.muted ? "Ton einschalten" : "Stumm schalten"}
                onClick={() => onToggleMute(channel.sourceId)}
              >
                <SpeakerIcon size={14} />
              </button>
            </div>
          </div>
        ))}
      </div>
      {channels.length === 0 && <p className="empty">Noch keine Audioquelle.</p>}
      {audioError && <p role="alert">{audioError}</p>}
    </section>
  );
}

export const AudioMixerPanel = memo(AudioMixerPanelImpl);
