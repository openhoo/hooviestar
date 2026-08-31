import { memo } from "react";
import type { MediaRuntimeState, SceneItem, Source, TextSource } from "../types";
import { MediaInspector } from "./MediaInspector";


export type ItemAction = "toggleVisible" | "toggleLocked" | "moveUp" | "moveDown";

interface SourceInspectorPanelProps {
  selectedSource: Source | null;
  selectedItem: SceneItem | null;
  mediaState: MediaRuntimeState | null;
  textError: string | null;
  onTextChange: (source: TextSource, event: React.ChangeEvent<HTMLTextAreaElement>) => void;
  onAudioField: (sourceId: string, field: "volume" | "muted", value: number | boolean) => void;
  getPendingField: <T>(sourceId: string, field: string, fallback: T) => T;
  onUpdateSource: (sourceId: string, changes: Partial<Source>) => Promise<void>;
  onSeek: (sourceId: string, positionSeconds: number) => Promise<unknown>;
  onSetPlaying: (sourceId: string, playing: boolean) => Promise<unknown>;
}

const SOURCE_TYPE_LABELS: Record<Source["type"], string> = {
  window: "Fensteraufnahme",
  display: "Monitoraufnahme",
  image: "Bild",
  text: "Text",
  media: "Medium",
  application_audio: "Anwendungs-Audio",
};

function SourceInspectorPanelImpl({
  selectedSource,
  selectedItem,
  mediaState,
  textError,
  onTextChange,
  onAudioField,
  getPendingField,
  onUpdateSource,
  onSeek,
  onSetPlaying,
}: SourceInspectorPanelProps) {
  return (
    <aside className="dock inspector-dock" aria-label="Eigenschaften">
      <div className="dock-title">
        <h2>Eigenschaften</h2>
      </div>
      {selectedSource ? (
        <div className="properties">
          <header className="source-summary">
            <span className="source-avatar" aria-hidden="true">{selectedSource.name.slice(0, 1).toUpperCase()}</span>
            <div>
              <h3 title={selectedSource.name}>{selectedSource.name}</h3>
              <p>{SOURCE_TYPE_LABELS[selectedSource.type]}</p>
            </div>
            <span className={selectedItem ? "placement-badge" : "placement-badge detached"}>
              {selectedItem ? "In Szene" : "Nicht in Szene"}
            </span>
          </header>
          {selectedSource.type === "text" && (
            <section className="property-group">
              <h3>Inhalt</h3>
              <label>Text<textarea key={selectedSource.id} defaultValue={getPendingField(selectedSource.id, "text", selectedSource.text)} onChange={(event) => onTextChange(selectedSource, event)} /></label>
              {textError && <p role="alert" className="source-message">{textError}</p>}
            </section>
          )}
          {"volume" in selectedSource && (
            <section className="property-group">
              <h3>Audio</h3>
              <label>Lautstärke <output>{Math.round(getPendingField(selectedSource.id, "volume", selectedSource.volume) * 100)} %</output><input type="range" min="0" max="1" step="0.01" value={getPendingField(selectedSource.id, "volume", selectedSource.volume)} onChange={(event) => onAudioField(selectedSource.id, "volume", Number(event.currentTarget.value))} /></label>
              <label className="check"><input type="checkbox" checked={getPendingField(selectedSource.id, "muted", selectedSource.muted)} onChange={(event) => onAudioField(selectedSource.id, "muted", event.currentTarget.checked)} /> Stumm</label>
            </section>
          )}
          {selectedSource.type === "media" && (
            <section className="property-group">
              <h3>Wiedergabe</h3>
              <MediaInspector
                source={selectedSource}
                mediaState={mediaState}
                onUpdateSource={onUpdateSource}
                onSeek={onSeek}
                onSetPlaying={onSetPlaying}
              />
            </section>
          )}
        </div>
      ) : (
        <div className="empty-state">
          <span className="empty-state-icon" aria-hidden="true">◇</span>
          <strong>Keine Quelle ausgewählt</strong>
          <p>Quelle links auswählen, um Inhalt, Audio oder Wiedergabe zu bearbeiten.</p>
        </div>
      )}
    </aside>
  );
}

export const SourceInspectorPanel = memo(SourceInspectorPanelImpl);
