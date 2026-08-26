import { memo } from "react";
import type { MediaRuntimeState, SceneItem, Source, TextSource } from "../types";
import { MediaInspector } from "./MediaInspector";

function sourceLabel(source: Source) {
  if (source.type === "application_audio") return "Anwendungs-Audio";
  if (source.type === "window") return "Fenster";
  if (source.type === "display") return "Monitor";
  if (source.type === "image") return "Bild";
  if (source.type === "text") return "Text";
  return "Medium";
}

export type ItemAction = "toggleVisible" | "toggleLocked" | "moveUp" | "moveDown";

interface SourceInspectorPanelProps {
  sources: Source[];
  selectedSourceId: string | null;
  selectedSource: Source | null;
  selectedItem: SceneItem | null;
  mediaState: MediaRuntimeState | null;
  itemError: string | null;
  textError: string | null;
  addButtonRef: React.RefObject<HTMLButtonElement | null>;
  onSelectSource: (sourceId: string) => void;
  onAddClick: () => void;
  onItemAction: (itemId: string, action: ItemAction) => void;
  onTextChange: (source: TextSource, event: React.ChangeEvent<HTMLTextAreaElement>) => void;
  onAudioField: (sourceId: string, field: "volume" | "muted", value: number | boolean) => void;
  getPendingField: <T>(sourceId: string, field: string, fallback: T) => T;
  onUpdateSource: (sourceId: string, changes: Partial<Source>) => Promise<void>;
  onSeek: (sourceId: string, positionSeconds: number) => Promise<unknown>;
  onSetPlaying: (sourceId: string, playing: boolean) => Promise<unknown>;
}

function SourceInspectorPanelImpl({
  sources,
  selectedSourceId,
  selectedSource,
  selectedItem,
  mediaState,
  itemError,
  textError,
  addButtonRef,
  onSelectSource,
  onAddClick,
  onItemAction,
  onTextChange,
  onAudioField,
  getPendingField,
  onUpdateSource,
  onSeek,
  onSetPlaying,
}: SourceInspectorPanelProps) {
  return (
    <aside className="panel inspector">
      <div className="panel-title">
        <h2>Quellen</h2>
        <button ref={addButtonRef} onClick={onAddClick}>Hinzufügen</button>
      </div>
      <ul className="source-list">
        {sources.map((source) => (
          <li key={source.id}>
            <button
              className={source.id === selectedSourceId ? "selected" : ""}
              onClick={() => onSelectSource(source.id)}
            >
              <span className="source-type">{sourceLabel(source)}</span>
              <strong>{source.name}</strong>
            </button>
          </li>
        ))}
      </ul>
      {selectedSource ? (
        <div className="properties">
          <h3>Eigenschaften</h3>
          <label>Name<input value={selectedSource.name} readOnly /></label>
          {selectedItem && (
            <>
            <div className="property-actions">
              <button onClick={() => onItemAction(selectedItem.id, "toggleVisible")}>{selectedItem.visible ? "Ausblenden" : "Einblenden"}</button>
              <button onClick={() => onItemAction(selectedItem.id, "toggleLocked")}>{selectedItem.locked ? "Entsperren" : "Sperren"}</button>
              {/* Gesperrte Items lehnt die Engine für Neuanordnung ab (apply-Guard). */}
              <button disabled={selectedItem.locked} title={selectedItem.locked ? "Element ist gesperrt" : undefined} onClick={() => onItemAction(selectedItem.id, "moveUp")}>Nach oben</button>
              <button disabled={selectedItem.locked} title={selectedItem.locked ? "Element ist gesperrt" : undefined} onClick={() => onItemAction(selectedItem.id, "moveDown")}>Nach unten</button>
            </div>
            {itemError && <p role="alert">{itemError}</p>}
            </>
          )}
          {selectedSource.type === "text" && (
            <>
              <label>Text<textarea key={selectedSource.id} defaultValue={getPendingField(selectedSource.id, "text", selectedSource.text)} onChange={(event) => onTextChange(selectedSource, event)} /></label>
              {textError && <p role="alert" className="source-message">{textError}</p>}
            </>
          )}
          {"volume" in selectedSource && (
            <>
              <label>Lautstärke <output>{Math.round(getPendingField(selectedSource.id, "volume", selectedSource.volume) * 100)} %</output><input type="range" min="0" max="1" step="0.01" value={getPendingField(selectedSource.id, "volume", selectedSource.volume)} onChange={(event) => onAudioField(selectedSource.id, "volume", Number(event.currentTarget.value))} /></label>
              <label className="check"><input type="checkbox" checked={getPendingField(selectedSource.id, "muted", selectedSource.muted)} onChange={(event) => onAudioField(selectedSource.id, "muted", event.currentTarget.checked)} /> Stumm</label>
            </>
          )}
          {selectedSource.type === "media" && (
            <MediaInspector
              source={selectedSource}
              mediaState={mediaState}
              onUpdateSource={onUpdateSource}
              onSeek={onSeek}
              onSetPlaying={onSetPlaying}
            />
          )}
        </div>
      ) : <p className="empty">Quelle auswählen, um Eigenschaften zu bearbeiten.</p>}
    </aside>
  );
}

export const SourceInspectorPanel = memo(SourceInspectorPanelImpl);
