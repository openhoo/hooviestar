import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { EngineCommand, LevelsEvent, MediaRuntimeState, ProjectV1, SourceEnumeration } from "./types";
import { parseEngineEvent, parseProjectV1 } from "./types";

type Listener = () => void;
interface EngineState {
  project: ProjectV1 | null;
  status: string;
  levels: LevelsEvent["entries"];
  mediaStates: Record<string, MediaRuntimeState>;
}
const listeners = new Set<Listener>();
let state: EngineState = {
  project: null,
  status: "Engine wird gestartet",
  levels: [],
  mediaStates: {},
};
let started = false;
function levelsEqual(a: LevelsEvent["entries"], b: LevelsEvent["entries"]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i];
    const y = b[i];
    if (x.sourceId !== y.sourceId || x.peak !== y.peak || x.rms !== y.rms) return false;
  }
  return true;
}

function mediaStatesEqual(a: Record<string, MediaRuntimeState>, b: Record<string, MediaRuntimeState>): boolean {
  if (a === b) return true;
  const keys = Object.keys(a);
  if (keys.length !== Object.keys(b).length) return false;
  for (const key of keys) {
    const x = a[key];
    const y = b[key];
    if (!y) return false;
    if (
      x.playing !== y.playing ||
      x.positionSeconds !== y.positionSeconds ||
      x.durationSeconds !== y.durationSeconds
    ) {
      return false;
    }
  }
  return true;
}

// Wertgleiche Ereignisse (Pegel-/Medienstatus ohne sichtbare Änderung) dürfen
// keinen Identitätswechsel erzeugen und damit kein Full-Tree-Re-Render auslösen.
function stateUnchanged(next: EngineState): boolean {
  return (
    next.project === state.project &&
    next.status === state.status &&
    levelsEqual(next.levels, state.levels) &&
    mediaStatesEqual(next.mediaStates, state.mediaStates)
  );
}
function publish(next: EngineState) { state = next; listeners.forEach((listener) => listener()); }

/** Status-Präfixe, die eine Fehlerlage signalisieren (siehe start()). */
export const STATUS_ERROR_PREFIXES = [
  "Engine-Fehler:",
  "Grafikfehler:",
  "Quelle nicht verfügbar:",
  "Audiowarnung:",
  "Medium nicht unterstützt:",
  "Hotkey-Fehler:",
  "Engine-Status nicht bestätigt:",
  "Engine nicht erreichbar:",
] as const;

/** „error“, solange der sichtbare Status auf eine Fehlerlage zeigt. */
export function statusTone(status: string): "ok" | "error" {
  return STATUS_ERROR_PREFIXES.some((prefix) => status.startsWith(prefix)) ? "error" : "ok";
}
export const engineStore = {
  subscribe(listener: Listener) { listeners.add(listener); return () => listeners.delete(listener); },
  getSnapshot() { return state; },
  async start() {
    if (started) return;
    started = true;
    try {
      const initial = parseProjectV1(await invoke("get_snapshot"));
      if (!initial) throw new Error("Ungültiger Engine-Snapshot");
      publish({ ...state, project: initial, status: "Program bereit" });
      await listen<unknown>("engine-event", ({ payload }) => {
        const event = parseEngineEvent(payload);
        if (!event) {
          console.warn("[hooviestar] unverständliches Engine-Event verworfen:", payload);
          return;
        }
        let status = state.status;
        if (event.type === "device_recovery") {
          status = event.phase === "started"
            ? "Grafikgerät wird neu gestartet"
            : event.phase === "failed"
              ? `Grafikfehler: ${event.detail ?? "unbekannt"}`
              : "Program bereit";
        } else if (event.type === "source_unavailable") {
          status = `Quelle nicht verfügbar: ${event.reason}`;
        } else if (event.type === "source_available") {
          status = "Program bereit";
        } else if (event.type === "audio_warning") {
          status = `Audiowarnung: ${event.message}`;
        } else if (
          event.type === "levels" &&
          status.startsWith("Audiowarnung:") &&
          event.entries.some((entry) => entry.peak > 0)
        ) {
          status = "Program bereit";
        } else if (
          event.type === "media_state" &&
          !event.state.playing &&
          status.startsWith("Audiowarnung:")
        ) {
          status = "Program bereit";
        } else if (event.type === "unsupported_media") {
          status = `Medium nicht unterstützt: ${event.reason}`;
        } else if (event.type === "hotkey_error") {
          status = `Hotkey-Fehler: ${event.message}`;
        } else if (event.type === "engine_error") {
          status = `Engine-Fehler: ${event.message}`;
        }
        const levels = event.type === "levels" ? event.entries : state.levels;
        const mediaStates =
          event.type === "media_state"
            ? { ...state.mediaStates, [event.sourceId]: event.state }
            : event.type === "snapshot"
              ? Object.fromEntries(
                  Object.entries(state.mediaStates).filter(([sourceId]) =>
                    event.project.sources.some((source) => source.id === sourceId),
                  ),
                )
              : state.mediaStates;
        const next: EngineState = {
          ...state,
          project: event.type === "snapshot" ? event.project : state.project,
          status,
          levels,
          mediaStates,
        };
        if (!stateUnchanged(next)) publish(next);
      });
      // Bereitschaftssignal: der Listener ist angehängt – vom Start verpasste
      // Hotkey-Fehler des Backends jetzt nachliefern (drainiert den Puffer).
      await invoke("engine_status").catch((error: unknown) => {
        // Wie der Snapshot-Pfad behandeln: Ein nicht bestätigter Status darf
        // nicht hinter grünem Programm-Status versickern. Startflag lösen,
        // sichtbaren Status melden – der nächste Mount versucht den
        // vollständigen Attach samt Nachlieferung erneut.
        started = false;
        publish({ ...state, status: `Engine-Status nicht bestätigt: ${String(error)}` });
      });
    } catch (error) {
      // Fehlgeschlagenen Start nicht dauerhaft festschreiben: Der nächste
      // start()-Aufruf (z. B. nach Remount) darf es erneut versuchen.
      started = false;
      publish({ ...state, status: `Engine nicht erreichbar: ${String(error)}` });
    }
  },
  async dispatch(command: EngineCommand) { await invoke("dispatch", { command }); },
  async enumerateSources() { return invoke<SourceEnumeration>("enumerate_sources"); },
  async selectPortalSources() { return invoke<SourceEnumeration>("select_portal_sources"); },
};
