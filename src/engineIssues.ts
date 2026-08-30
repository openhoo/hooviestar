import type { EngineEvent, ProjectV1 } from "./types";

function setLatest(map: Map<string, string>, key: string, message: string) {
  map.delete(key);
  map.set(key, message);
}

function latest(map: Map<string, string>): string | null {
  let value: string | null = null;
  for (const entry of map.values()) value = entry;
  return value;
}

/**
 * Tracks independent engine failures instead of letting an unrelated success
 * event paint the whole application green. Maps retain one issue per source or
 * scene; matching recovery events clear only their own issue.
 */
export class EngineIssueTracker {
  private startup: string | null = "Engine wird gestartet";
  private engine: string | null = null;
  private device: string | null = null;
  private audioDevice: string | null = null;
  private audioFlow: string | null = null;
  private readonly sources = new Map<string, string>();
  private readonly media = new Map<string, string>();
  private readonly hotkeys = new Map<string, string>();

  setStartup(message: string | null) {
    this.startup = message;
  }

  clearHotkey(sceneId: string) {
    this.hotkeys.delete(sceneId);
  }

  record(event: EngineEvent) {
    switch (event.type) {
      case "snapshot":
        this.prune(event.project);
        break;
      case "device_recovery":
        this.device = event.phase === "started"
          ? "Grafikgerät wird neu gestartet"
          : event.phase === "failed"
            ? `Grafikfehler: ${event.detail ?? "unbekannt"}`
            : null;
        break;
      case "source_unavailable":
        setLatest(
          this.sources,
          event.sourceId,
          `Quelle nicht verfügbar: ${event.reason}`,
        );
        break;
      case "source_available":
        this.sources.delete(event.sourceId);
        this.media.delete(event.sourceId);
        break;
      case "audio_warning":
        if (event.kind === "device_invalidated") {
          this.audioDevice = `Audiowarnung: ${event.message}`;
        } else {
          this.audioFlow = `Audiowarnung: ${event.message}`;
        }
        break;
      case "audio_recovered":
        this.audioDevice = null;
        break;
      case "levels":
        if (event.entries.some((entry) => entry.peak > 0)) this.audioFlow = null;
        break;
      case "media_state":
        this.media.delete(event.sourceId);
        if (!event.state.playing) this.audioFlow = null;
        break;
      case "unsupported_media":
        setLatest(
          this.media,
          event.sourceId,
          `Medium nicht unterstützt: ${event.reason}`,
        );
        break;
      case "hotkey_error":
        setLatest(
          this.hotkeys,
          event.sceneId,
          `Hotkey-Fehler: ${event.message}`,
        );
        break;
      case "engine_error":
        this.engine = `Engine-Fehler: ${event.message}`;
        break;
    }
  }

  status(): string {
    return this.startup
      ?? this.engine
      ?? this.device
      ?? latest(this.hotkeys)
      ?? latest(this.media)
      ?? latest(this.sources)
      ?? this.audioDevice
      ?? this.audioFlow
      ?? "Program bereit";
  }

  private prune(project: ProjectV1) {
    const sourceIds = new Set(project.sources.map((source) => source.id));
    const sceneIds = new Set(project.scenes.map((scene) => scene.id));
    for (const sourceId of this.sources.keys()) {
      if (!sourceIds.has(sourceId)) this.sources.delete(sourceId);
    }
    for (const sourceId of this.media.keys()) {
      if (!sourceIds.has(sourceId)) this.media.delete(sourceId);
    }
    for (const sceneId of this.hotkeys.keys()) {
      if (!sceneIds.has(sceneId)) this.hotkeys.delete(sceneId);
    }
  }
}
