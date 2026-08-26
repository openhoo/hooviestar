import { useCallback, useEffect, useRef, useState } from "react";
import { engineStore } from "../engineStore";
import type { EngineCommand, ProjectV1 } from "../types";

/**
 * Brücke zwischen Mixer/Eigenschaften und den Audio-Engine-Befehlen:
 * Lautstärke/Stumm laufen optimistisch über das Feld-Overlay und werden pro
 * Animation-Frame koalesziert an die Engine geschickt (letzter Schreibzugriff
 * gewinnt). Das Feld-Overlay (`pendingSourceFieldsRef`) bleibt beim Aufrufer –
 * updateSource liest und beschreibt es, der Projekt-Abgleich stutzt es – und
 * wird dieser Hook nur als Referenz übergeben; der IPC-Flush samt
 * Fehlerkanal gehört vollständig hierher.
 */
export function useAudioFieldBridge(
  pendingSourceFieldsRef: React.RefObject<Map<string, Record<string, unknown>>>,
) {
  const [audioError, setAudioError] = useState<string | null>(null);
  // IPC-Koaleszierung für Lautstärke/Stumm: ausstehende Befehle je Quelle+Feld,
  // geflusht einmal pro Animation-Frame (letzter Schreibzugriff gewinnt).
  const audioPendingDispatchRef = useRef(new Map<string, EngineCommand>());
  const audioFlushFrameRef = useRef<number | null>(null);

  function dispatchAudioBatch(batch: Map<string, EngineCommand>) {
    for (const [key, command] of batch) {
      if (command.type !== "set_audio_volume" && command.type !== "set_audio_muted") continue;
      const value = command.type === "set_audio_volume" ? command.volume : command.muted;
      void engineStore.dispatch(command).catch((error: unknown) => {
        // Fehlschlag sichtbar machen und das Overlay selbst heilen lassen:
        // der Pending-Eintrag fällt weg, sofern nicht inzwischen ein neuerer
        // Wert geschrieben wurde (dann bleibt dessen Optimistik erhalten).
        const separator = key.lastIndexOf(":");
        const sourceId = key.slice(0, separator);
        const field = key.slice(separator + 1);
        const pending = pendingSourceFieldsRef.current.get(sourceId);
        if (pending && Object.is(pending[field], value)) {
          delete pending[field];
          if (Object.keys(pending).length === 0) pendingSourceFieldsRef.current.delete(sourceId);
        }
        setAudioError(String(error));
      });
    }
  }

  const flushAudioFields = useCallback(() => {
    audioFlushFrameRef.current = null;
    const batch = audioPendingDispatchRef.current;
    if (batch.size === 0) return;
    audioPendingDispatchRef.current = new Map();
    dispatchAudioBatch(batch);
  }, [pendingSourceFieldsRef]);

  const setAudioField = useCallback((sourceId: string, field: "volume" | "muted", value: number | boolean) => {
    const pending = pendingSourceFieldsRef.current.get(sourceId);
    pendingSourceFieldsRef.current.set(sourceId, { ...pending, [field]: value });
    // Overlay sofort setzen; der IPC wird pro Animation-Frame koalesziert,
    // damit Slider-Ticks sich gegenseitig im Pending-Map überschreiben.
    audioPendingDispatchRef.current.set(
      `${sourceId}:${field}`,
      field === "volume"
        ? { type: "set_audio_volume", sourceId, volume: value as number }
        : { type: "set_audio_muted", sourceId, muted: value as boolean },
    );
    if (audioFlushFrameRef.current === null) {
      audioFlushFrameRef.current = requestAnimationFrame(flushAudioFields);
    }
  }, [flushAudioFields, pendingSourceFieldsRef]);

  // Ausstehende Audio-Befehle beim Aushängen synchron flushen, damit die
  // letzten Lautstärke-/Stumm-Änderungen nicht mit dem rAF verloren gehen.
  useEffect(() => () => {
    if (audioFlushFrameRef.current !== null) cancelAnimationFrame(audioFlushFrameRef.current);
    const batch = audioPendingDispatchRef.current;
    audioPendingDispatchRef.current = new Map();
    dispatchAudioBatch(batch);
  }, [pendingSourceFieldsRef]);

  const pendingField = useCallback(<T,>(sourceId: string, field: string, fallback: T): T => {
    const pending = pendingSourceFieldsRef.current.get(sourceId)?.[field];
    return pending == null ? fallback : pending as T;
  }, [pendingSourceFieldsRef]);

  // Snapshot statt Render-Closure: der Kanal kann zwischen Renders verschwunden sein.
  const toggleMixerMute = useCallback((sourceId: string) => {
    const source = engineStore.getSnapshot().project?.sources.find((entry) => entry.id === sourceId);
    if (!source || !("muted" in source)) return;
    const muted = pendingField(sourceId, "muted", source.muted);
    setAudioField(sourceId, "muted", !muted);
  }, [pendingField, setAudioField]);

  const setMixerVolume = useCallback((sourceId: string, volume: number) => {
    setAudioField(sourceId, "volume", volume);
  }, [setAudioField]);

  // Projekt-Abgleich: bestätigte Felder aus dem Overlay werfen, damit die
  // Optimistik nicht länger als nötig am Snapshot hängt.
  const prunePendingFields = useCallback((project: ProjectV1) => {
    const pendingBySource = pendingSourceFieldsRef.current;
    for (const source of project.sources) {
      const pending = pendingBySource.get(source.id);
      if (!pending) continue;
      for (const key of Object.keys(pending)) {
        if (pending[key] === (source as unknown as Record<string, unknown>)[key]) delete pending[key];
      }
      if (Object.keys(pending).length === 0) pendingBySource.delete(source.id);
    }
  }, [pendingSourceFieldsRef]);

  return { audioError, setAudioField, toggleMixerMute, setMixerVolume, pendingField, prunePendingFields };
}
