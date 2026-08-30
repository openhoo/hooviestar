import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { EngineCommand, LevelsEvent, MediaRuntimeState, ProjectV1, SourceEnumeration } from "./types";
import { parseEngineEvent, parseProjectV1 } from "./types";
import { EngineIssueTracker } from "./engineIssues";

type Listener = () => void;
interface EngineState {
  project: ProjectV1 | null;
  status: string;
  levels: LevelsEvent["entries"];
  mediaStates: Record<string, MediaRuntimeState>;
}
const listeners = new Set<Listener>();
const issues = new EngineIssueTracker();
let state: EngineState = {
  project: null,
  status: issues.status(),
  levels: [],
  mediaStates: {},
};
let started = false;
let starting: Promise<void> | null = null;
function levelsEqual(a: LevelsEvent["entries"], b: LevelsEvent["entries"]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i]!;
    const y = b[i]!;
    if (x.sourceId !== y.sourceId || x.peak !== y.peak || x.rms !== y.rms) return false;
  }
  return true;
}

function mediaStatesEqual(a: Record<string, MediaRuntimeState>, b: Record<string, MediaRuntimeState>): boolean {
  if (a === b) return true;
  const keys = Object.keys(a);
  if (keys.length !== Object.keys(b).length) return false;
  for (const key of keys) {
    const x = a[key]!;
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

function publishStatus() {
  const status = issues.status();
  if (status !== state.status) publish({ ...state, status });
}

function handleEnginePayload(payload: unknown) {
  const event = parseEngineEvent(payload);
  if (!event) {
    console.warn("[hooviestar] unverständliches Engine-Event verworfen:", payload);
    return;
  }
  issues.record(event);
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
    status: issues.status(),
    levels,
    mediaStates,
  };
  if (!stateUnchanged(next)) publish(next);
}

async function connect() {
  let unlisten: (() => void) | null = null;
  issues.setStartup("Engine wird gestartet");
  publishStatus();
  try {
    const initial = parseProjectV1(await invoke("get_snapshot"));
    if (!initial) throw new Error("Ungültiger Engine-Snapshot");
    issues.record({ type: "snapshot", project: initial });
    publish({ ...state, project: initial, status: issues.status() });
    unlisten = await listen<unknown>("engine-event", ({ payload }) => handleEnginePayload(payload));
    // Bereitschaftssignal: der Listener ist angehängt – vom Start verpasste
    // Hotkey-Fehler des Backends jetzt nachliefern (drainiert den Puffer).
    try {
      await invoke("engine_status");
      started = true;
      issues.setStartup(null);
      publishStatus();
    } catch (error) {
      // Listener vor Retry entfernen; sonst vervielfacht jeder fehlgeschlagene
      // Bereitschafts-Handshake alle späteren Engine-Ereignisse.
      unlisten();
      issues.setStartup(`Engine-Status nicht bestätigt: ${String(error)}`);
      publishStatus();
    }
  } catch (error) {
    // Auch Fehler nach erfolgreichem listen() dürfen keinen halben Anschluss
    // hinterlassen. Der nächste start()-Aufruf versucht den kompletten Ablauf.
    unlisten?.();
    issues.setStartup(`Engine nicht erreichbar: ${String(error)}`);
    publishStatus();
  }
}

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
  "Vorschaufehler:",
  "Fensterfehler:",
] as const;

/** „error“, solange der sichtbare Status auf eine Fehlerlage zeigt. */
export function statusTone(status: string): "ok" | "error" {
  return STATUS_ERROR_PREFIXES.some((prefix) => status.startsWith(prefix)) ? "error" : "ok";
}
export const engineStore = {
  subscribe(listener: Listener) { listeners.add(listener); return () => listeners.delete(listener); },
  getSnapshot() { return state; },
  start() {
    if (started) return Promise.resolve();
    if (starting) return starting;
    const attempt = connect().finally(() => {
      if (starting === attempt) starting = null;
    });
    starting = attempt;
    return attempt;
  },
  async dispatch(command: EngineCommand) {
    await invoke("dispatch", { command });
    if (command.type === "set_scene_hotkey") {
      issues.clearHotkey(command.sceneId);
      publishStatus();
    }
  },
  async enumerateSources() { return invoke<SourceEnumeration>("enumerate_sources"); },
  async selectPortalSources() { return invoke<SourceEnumeration>("select_portal_sources"); },
};
