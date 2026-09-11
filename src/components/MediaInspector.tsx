import { memo, useEffect, useRef, useState } from "react";
import type { MediaRuntimeState, MediaSource, Source } from "../types";
import { runGuarded } from "../guarded";

interface MediaInspectorProps {
  source: MediaSource;
  mediaState: MediaRuntimeState | null;
  onUpdateSource: (sourceId: string, changes: Partial<Source>) => Promise<void>;
  onSeek: (sourceId: string, positionSeconds: number) => Promise<unknown>;
  onSetPlaying: (sourceId: string, playing: boolean) => Promise<unknown>;
}

function MediaInspectorImpl({ source, mediaState, onUpdateSource, onSeek, onSetPlaying }: MediaInspectorProps) {
  const [positionEditing, setPositionEditing] = useState(false);
  const [positionDraft, setPositionDraft] = useState("0");
  const [positionError, setPositionError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const positionBaselineRef = useRef<string | null>(null);
  const sourceIdRef = useRef(source.id);
  sourceIdRef.current = source.id;

  // Local input/error state belongs to one media source. Without resetting it
  // on selection changes, a failed seek/action and an in-progress position
  // draft leak into the next selected source.
  useEffect(() => {
    positionBaselineRef.current = null;
    setPositionEditing(false);
    setPositionDraft(String(Math.round(mediaState?.positionSeconds ?? 0)));
    setPositionError(null);
    setActionError(null);
  }, [source.id]);

  // Macht Rejektionen der Sofort-Aktionen sichtbar, statt sie unbehandelt zu
  // lassen; die Meldung wird beim nächsten Versuch verworfen (Helfer: ../guarded).
  function guardAction(flow: Promise<unknown>) {
    const sourceId = source.id;
    void runGuarded(
      () => flow,
      (message) => {
        if (sourceIdRef.current === sourceId) setActionError(message);
      },
    );
  }

  function togglePlaying() {
    guardAction(onSetPlaying(source.id, !(mediaState?.playing ?? true)));
  }

  function focusPosition(event: React.FocusEvent<HTMLInputElement>) {
    const value = event.currentTarget.value;
    positionBaselineRef.current = value;
    setPositionDraft(value);
    setPositionEditing(true);
  }

  function draftPosition(event: React.ChangeEvent<HTMLInputElement>) {
    setPositionDraft(event.currentTarget.value);
  }

  function submitPositionOnEnter(event: React.KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter") event.currentTarget.blur();
  }

  // Wie guardAction: die Rejektion des Suchlaufs sichtbar machen, statt sie
  // unbehandelt zu lassen; unveränderte/leere/negative Eingaben ignorieren.
  function commitPosition() {
    setPositionError(null);
    const seconds = Number(positionDraft);
    const unchanged =
      positionBaselineRef.current !== null && seconds === Number(positionBaselineRef.current);
    positionBaselineRef.current = null;
    setPositionEditing(false);
    if (positionDraft.trim() === "" || !Number.isFinite(seconds) || seconds < 0 || unchanged) return;
    const sourceId = source.id;
    onSeek(sourceId, seconds).catch((error: unknown) => {
      if (sourceIdRef.current === sourceId) setPositionError(String(error));
    });
  }

  function setLoop(checked: boolean) {
    guardAction(onUpdateSource(source.id, { loop: checked }));
  }

  function setContinueWhenHidden(checked: boolean) {
    guardAction(onUpdateSource(source.id, { continueWhenHidden: checked }));
  }

  function setRestartOnShow(checked: boolean) {
    guardAction(onUpdateSource(source.id, { restartOnShow: checked }));
  }

  return (
    <div className="media-controls">
      <button type="button" onClick={togglePlaying}>{mediaState?.playing === false ? "Wiedergabe" : "Pause"}</button>
      <label>Position (Sekunden)<input type="number" min="0" step="1" value={positionEditing ? positionDraft : Math.round(mediaState?.positionSeconds ?? 0)} onFocus={focusPosition} onChange={draftPosition} onKeyDown={submitPositionOnEnter} onBlur={commitPosition} /></label>
      {positionError && <p role="alert" className="source-message">{positionError}</p>}
      {actionError && <p role="alert" className="source-message">{actionError}</p>}
      <label className="check"><input type="checkbox" checked={source.loop} onChange={(event) => setLoop(event.currentTarget.checked)} /> Wiederholen</label>
      <label className="check"><input type="checkbox" checked={source.continueWhenHidden} onChange={(event) => setContinueWhenHidden(event.currentTarget.checked)} /> Versteckt weiterlaufen</label>
      <label className="check"><input type="checkbox" checked={source.restartOnShow} onChange={(event) => setRestartOnShow(event.currentTarget.checked)} /> Beim Einblenden neu starten</label>
    </div>
  );
}

export const MediaInspector = memo(MediaInspectorImpl);
