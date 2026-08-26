import { memo } from "react";
import type { Source } from "../types";

type LevelEntry = { sourceId: string; peak: number; rms: number };

interface AudioMixerPanelProps {
  sources: Source[];
  levels: LevelEntry[];
  audioError: string | null;
  getPendingField: <T>(sourceId: string, field: string, fallback: T) => T;
  onToggleMute: (source: Source) => void;
}

function AudioMixerPanelImpl({ sources, levels, audioError, getPendingField, onToggleMute }: AudioMixerPanelProps) {
  function levelWidth(sourceId: string): number {
    const peak = levels.find((entry) => entry.sourceId === sourceId)?.peak ?? 0;
    return Math.min(100, Math.round(peak * 100));
  }

  function mixerMuted(source: Source): boolean {
    return getPendingField(source.id, "muted", "muted" in source ? source.muted : false);
  }

  const audioSources = sources.filter((source) => "volume" in source);
  return (
    <section className="panel mixer" aria-label="Audiomixer">
      <div className="panel-title"><h2>Audiomixer</h2><span>48 kHz · Stereo</span></div>
      <div className="mixer-grid">
        {audioSources.map((source) => (
          <div className="channel" key={source.id}>
            <strong>{source.name}</strong>
            <div className="meter" aria-label={`Pegel ${source.name}`}><i style={{ width: `${levelWidth(source.id)}%` }} /></div>
            <button onClick={() => onToggleMute(source)}>{mixerMuted(source) ? "Ton an" : "Stumm"}</button>
          </div>
        ))}
        {audioSources.length === 0 && <p className="empty">Noch keine Audioquelle.</p>}
      </div>
      {audioError && <p role="alert">{audioError}</p>}
    </section>
  );
}

export const AudioMixerPanel = memo(AudioMixerPanelImpl);
