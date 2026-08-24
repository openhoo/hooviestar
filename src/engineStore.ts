import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { EngineCommand, EngineEvent, LevelsEvent, MediaRuntimeState, ProjectV1, SourceEnumeration } from "./types";
import { parseEngineEvent, parseProjectV1 } from "./types";

type Listener = () => void;
interface EngineState {
  project: ProjectV1 | null;
  status: string;
  lastEvent: EngineEvent | null;
  levels: LevelsEvent["entries"];
  mediaStates: Record<string, MediaRuntimeState>;
}
const listeners = new Set<Listener>();
let state: EngineState = {
  project: null,
  status: "Engine wird gestartet",
  lastEvent: null,
  levels: [],
  mediaStates: {},
};
let started = false;
function publish(next: EngineState) { state = next; listeners.forEach((listener) => listener()); }

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
        if (!event) return;
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
            : state.mediaStates;
        publish({
          ...state,
          project: event.type === "snapshot" ? event.project : state.project,
          status,
          lastEvent: event,
          levels,
          mediaStates,
        });
      });
    } catch (error) {
      publish({ ...state, status: `Engine nicht erreichbar: ${String(error)}` });
    }
  },
  async dispatch(command: EngineCommand) { await invoke("dispatch", { command }); },
  async enumerateSources() { return invoke<SourceEnumeration>("enumerate_sources"); },
  async selectPortalSources() { return invoke<SourceEnumeration>("select_portal_sources"); },
};
