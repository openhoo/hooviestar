import { memo, useEffect, useRef, useState } from "react";
import type { MediaRuntimeState, SceneItem, Source, TextSource, Transform } from "../types";
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
  onUpdateTransform: (itemId: string, transform: Transform, expected?: Transform) => Promise<void> | void;
  onTransformError?: (message: string | null) => void;
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

interface SourceDetail {
  label: string;
  value: string;
  title?: string;
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

function sourceDetails(source: Source): SourceDetail[] {
  switch (source.type) {
    case "window":
      return [
        { label: "Fenster", value: source.binding.windowTitle },
        { label: "Anwendung", value: fileName(source.binding.processPath), title: source.binding.processPath },
      ];
    case "display":
      return [
        { label: "Output-ID", value: String(source.binding.outputId) },
        { label: "Adapter", value: source.binding.adapterLuid },
      ];
    case "image":
    case "media":
      return [{ label: "Datei", value: fileName(source.path), title: source.path }];
    case "application_audio":
      return [
        { label: "Anwendung", value: fileName(source.binding.processPath), title: source.binding.processPath },
        { label: "Sitzung", value: source.binding.sessionGroupingId },
      ];
    case "text":
      return [];
  }
}

type NumericTransformField = "x" | "y" | "width" | "height" | "rotationDegrees"
  | "cropTop" | "cropRight" | "cropBottom" | "cropLeft" | "opacity";

interface TransformFieldDefinition {
  field: NumericTransformField;
  label: string;
  step: string;
  min?: number;
  max?: number;
}

const TRANSFORM_FIELDS: TransformFieldDefinition[] = [
  { field: "x", label: "X", step: "1" },
  { field: "y", label: "Y", step: "1" },
  { field: "width", label: "Breite", step: "1", min: 1 },
  { field: "height", label: "Höhe", step: "1", min: 1 },
  { field: "rotationDegrees", label: "Drehung", step: "1" },
  { field: "cropTop", label: "Crop oben", step: "1", min: 0 },
  { field: "cropRight", label: "Crop rechts", step: "1", min: 0 },
  { field: "cropBottom", label: "Crop unten", step: "1", min: 0 },
  { field: "cropLeft", label: "Crop links", step: "1", min: 0 },
  { field: "opacity", label: "Deckkraft", step: "0.01", min: 0, max: 1 },
];

function transformFieldValues(transform: Transform): Record<NumericTransformField, string> {
  return {
    x: String(transform.x),
    y: String(transform.y),
    width: String(transform.width),
    height: String(transform.height),
    rotationDegrees: String(transform.rotationDegrees),
    cropTop: String(transform.cropTop),
    cropRight: String(transform.cropRight),
    cropBottom: String(transform.cropBottom),
    cropLeft: String(transform.cropLeft),
    opacity: String(transform.opacity),
  };
}

function normalizeInspectorTransform(transform: Transform): Transform {
  const width = Math.max(1, transform.width);
  const height = Math.max(1, transform.height);
  const cropLeft = Math.min(Math.max(0, transform.cropLeft), Math.max(0, width - 1));
  const cropRight = Math.min(Math.max(0, transform.cropRight), Math.max(0, width - cropLeft - 1));
  const cropTop = Math.min(Math.max(0, transform.cropTop), Math.max(0, height - 1));
  const cropBottom = Math.min(Math.max(0, transform.cropBottom), Math.max(0, height - cropTop - 1));
  return {
    ...transform,
    width,
    height,
    cropLeft,
    cropRight,
    cropTop,
    cropBottom,
    opacity: Math.min(1, Math.max(0, transform.opacity)),
  };
}

interface TransformEditorProps {
  item: SceneItem;
  onUpdate: (itemId: string, transform: Transform, expected?: Transform) => Promise<void> | void;
  onError?: (message: string | null) => void;
}

function TransformEditor({ item, onUpdate, onError }: TransformEditorProps) {
  const [values, setValues] = useState(() => transformFieldValues(item.transform));
  const [error, setError] = useState<string | null>(null);
  const editingFieldRef = useRef<NumericTransformField | null>(null);
  const skipBlurCommitRef = useRef(false);
  const itemIdRef = useRef(item.id);
  const itemRef = useRef(item);
  itemRef.current = item;
  const latestTransformRef = useRef(item.transform);
  const editGenerationRef = useRef(0);
  useEffect(() => {
    const itemChanged = itemIdRef.current !== item.id;
    itemIdRef.current = item.id;
    if (itemChanged) editGenerationRef.current += 1;
    if (itemChanged || !editingFieldRef.current) {
      latestTransformRef.current = item.transform;
      setValues(transformFieldValues(item.transform));
    }
  }, [item.id, item.transform]);

  function clearError() {
    setError(null);
    onError?.(null);
  }

  function commit(field: NumericTransformField) {
    const value = Number(values[field]);
    if (!Number.isFinite(value)) {
      const message = `${field} muss eine Zahl sein.`;
      setError(message);
      onError?.(message);
      setValues(transformFieldValues(latestTransformRef.current));
      return;
    }
    const base = latestTransformRef.current;
    const next = normalizeInspectorTransform({ ...base, [field]: value });
    if (next[field] === base[field]) {
      clearError();
      return;
    }
    const editedItemId = item.id;
    const editGeneration = editGenerationRef.current;
    latestTransformRef.current = next;
    clearError();
    const handleError = (reason: unknown) => {
      if (itemIdRef.current !== editedItemId || latestTransformRef.current !== next) return;
      const authoritative = itemRef.current.id === editedItemId
        ? itemRef.current.transform
        : item.transform;
      latestTransformRef.current = authoritative;
      if (editGenerationRef.current !== editGeneration) return;
      const message = String(reason);
      setError(message);
      onError?.(message);
      setValues(transformFieldValues(authoritative));
    };
    try {
      void Promise.resolve(onUpdate(editedItemId, next, base)).catch(handleError);
    } catch (reason) {
      handleError(reason);
    }
  }

  return (
    <div className="transform-editor">
      <div className="transform-grid">
        {TRANSFORM_FIELDS.map((definition) => (
          <label key={definition.field}>
            {definition.label}
            <input
              type="number"
              inputMode="decimal"
              step={definition.step}
              min={definition.min}
              max={definition.max}
              value={values[definition.field]}
              disabled={item.locked}
              onFocus={() => { editingFieldRef.current = definition.field; }}
              onChange={(event) => {
                const value = event.currentTarget.value;
                editGenerationRef.current += 1;
                setValues((current) => ({ ...current, [definition.field]: value }));
                clearError();
              }}
              onBlur={() => {
                if (editingFieldRef.current === definition.field) editingFieldRef.current = null;
                if (skipBlurCommitRef.current) {
                  skipBlurCommitRef.current = false;
                  return;
                }
                commit(definition.field);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  event.currentTarget.blur();
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  skipBlurCommitRef.current = true;
                  editGenerationRef.current += 1;
                  setValues(transformFieldValues(latestTransformRef.current));
                  editingFieldRef.current = null;
                  event.currentTarget.blur();
                }
              }}
              aria-label={`${definition.label} der Quelle`}
            />
          </label>
        ))}
      </div>
      <p className="transform-help">
        Pfeile bewegen 1 px, Shift 10 px; Alt + Pfeile ändern die Größe. {item.locked ? "Quelle ist gesperrt." : "Esc bricht eine laufende Änderung ab."}
      </p>
      {error && <p className="field-error" role="alert">{error}</p>}
    </div>
  );
}

function SourceInspectorPanelImpl({
  selectedSource,
  selectedItem,
  mediaState,
  textError,
  onTextChange,
  onAudioField,
  getPendingField,
  onUpdateSource,
  onUpdateTransform,
  onTransformError,
  onSeek,
  onSetPlaying,
}: SourceInspectorPanelProps) {
  const [sourceNameError, setSourceNameError] = useState<string | null>(null);
  const details = selectedSource ? sourceDetails(selectedSource) : [];

  useEffect(() => setSourceNameError(null), [selectedSource?.id]);

  function commitSourceName(event: React.FocusEvent<HTMLInputElement>) {
    if (!selectedSource) return;
    const input = event.currentTarget;
    const name = input.value.trim();
    setSourceNameError(null);
    if (!name) {
      input.value = selectedSource.name;
      setSourceNameError("Quellenname darf nicht leer sein.");
      return;
    }
    if (name === selectedSource.name) {
      input.value = selectedSource.name;
      return;
    }
    void onUpdateSource(selectedSource.id, { name }).catch((error: unknown) => {
      if (!input.isConnected) return;
      input.value = selectedSource.name;
      setSourceNameError(`Quellenname konnte nicht gespeichert werden: ${String(error)}`);
    });
  }

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
              <label className="source-name-field">
                <span className="sr-only">Quellenname</span>
                <input
                  key={`${selectedSource.id}:${selectedSource.name}`}
                  className="source-name-input"
                  defaultValue={selectedSource.name}
                  aria-label="Quellenname"
                  aria-invalid={sourceNameError ? true : undefined}
                  aria-describedby={sourceNameError ? "source-name-error" : undefined}
                  onBlur={commitSourceName}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      event.currentTarget.blur();
                    } else if (event.key === "Escape") {
                      event.preventDefault();
                      event.currentTarget.value = selectedSource.name;
                      event.currentTarget.blur();
                    }
                  }}
                />
              </label>
              <p>{SOURCE_TYPE_LABELS[selectedSource.type]}</p>
            </div>
            <span className={selectedItem ? "placement-badge" : "placement-badge detached"}>
              {selectedItem ? "In Szene" : "Nicht in Szene"}
            </span>
          </header>
          {sourceNameError && <p id="source-name-error" className="source-message" role="alert">{sourceNameError}</p>}
          {details.length > 0 && (
            <section className="property-group">
              <h3>Quelle</h3>
              <dl className="source-details">
                {details.map((detail) => (
                  <div key={detail.label}>
                    <dt>{detail.label}</dt>
                    <dd title={detail.title}>{detail.value}</dd>
                  </div>
                ))}
              </dl>
            </section>
          )}
          {selectedItem && (
            <section className="property-group">
              <h3>Transform</h3>
              <TransformEditor item={selectedItem} onUpdate={onUpdateTransform} onError={onTransformError} />
            </section>
          )}
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
