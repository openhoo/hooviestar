/**
 * Engine-Grenze – exakte Spiegelbilder von `EngineCommand` und `EngineEvent`
 * aus `hooviestar-engine/src/engine.rs` (Tag `type`, snake_case Varianten,
 * camelCase Felder).
 *
 * Die Engine ist autoritativ: Befehle werden atomar angewendet, danach liefert
 * ein neues Snapshot-Event den verbindlichen Zustand. Das UI hält keine zweite
 * State-Maschine über persistierten Daten.
 */
import type {
  AudioSessionBinding,
  DisplayBinding,
  OutputConfig,
  ProjectV1,
  Source,
  Transform,
  Uuid,
  WindowBinding,
} from "./project";

export type SourceCandidate =
  | { type: "window"; runtimeId: string; name: string; binding: WindowBinding }
  | { type: "display"; runtimeId: string; name: string; binding: DisplayBinding }
  | {
      type: "application_audio";
      runtimeId: string;
      name: string;
      binding: AudioSessionBinding;
    };

export interface SourceEnumeration {
  candidates: SourceCandidate[];
  portalSelectionRequired: boolean;
  message: string | null;
}

/* ------------------------------------------------------------------ */
/* EngineCommand                                                       */
/* ------------------------------------------------------------------ */

export type EngineCommand =
  /** Globale Quelle anlegen; noch kein Szenenbezug. */
  | { type: "add_source"; source: Source }
  /** Globale Quelle entfernen; Engine entfernt referenzierende Items kaskadierend. */
  | { type: "remove_source"; sourceId: Uuid }
  /** Eigenschaften einer bestehenden Quelle vollständig ersetzen. */
  | { type: "update_source"; source: Source }
  | { type: "add_scene"; sceneId: Uuid; name: string }
  | { type: "remove_scene"; sceneId: Uuid }
  | { type: "rename_scene"; sceneId: Uuid; name: string }
  | { type: "set_active_scene"; sceneId: Uuid }
  /** Transaktional in der Engine: neuer Hotkey zuerst registrieren. */
  | { type: "set_scene_hotkey"; sceneId: Uuid; hotkey: string | null }
  /** Item am Ende der Zeichenreihenfolge (oben) einfügen. */
  | { type: "add_scene_item"; sceneId: Uuid; itemId: Uuid; sourceId: Uuid; transform: Transform }
  | { type: "remove_scene_item"; sceneId: Uuid; itemId: Uuid }
  | { type: "set_item_visible"; sceneId: Uuid; itemId: Uuid; visible: boolean }
  | { type: "set_item_locked"; sceneId: Uuid; itemId: Uuid; locked: boolean }
  /** Zielindex in der Zeichenreihenfolge; 0 = ganz unten. */
  | { type: "reorder_scene_item"; sceneId: Uuid; itemId: Uuid; index: number }
  | { type: "set_transform"; sceneId: Uuid; itemId: Uuid; transform: Transform }
  | { type: "set_output_config"; output: OutputConfig }
  | { type: "set_media_playing"; sourceId: Uuid; playing: boolean }
  /** Absolute Position in Sekunden. */
  | { type: "media_seek"; sourceId: Uuid; positionSeconds: number }
  | { type: "set_audio_volume"; sourceId: Uuid; volume: number }
  | { type: "set_audio_muted"; sourceId: Uuid; muted: boolean };

export const ENGINE_COMMAND_TYPES = [
  "add_source",
  "remove_source",
  "update_source",
  "add_scene",
  "remove_scene",
  "rename_scene",
  "set_active_scene",
  "set_scene_hotkey",
  "add_scene_item",
  "remove_scene_item",
  "set_item_visible",
  "set_item_locked",
  "reorder_scene_item",
  "set_transform",
  "set_output_config",
  "set_media_playing",
  "media_seek",
  "set_audio_volume",
  "set_audio_muted",
] as const;

export type EngineCommandType = (typeof ENGINE_COMMAND_TYPES)[number];

/* ------------------------------------------------------------------ */
/* EngineEvent                                                         */
/* ------------------------------------------------------------------ */

/** Autoritative Projektübernahme nach jeder bestätigten Mutation. */
export interface SnapshotEvent {
  type: "snapshot";
  project: ProjectV1;
}

/** Quelle wieder erreichbar und gebunden. */
export interface SourceAvailableEvent {
  type: "source_available";
  sourceId: Uuid;
}

/**
 * Quelle minimiert/geschlossen/unsichtbar/geschützt oder Medium nicht
 * dekodierbar; Program rendert stattdessen die Hinweistafel.
 */
export interface SourceUnavailableEvent {
  type: "source_unavailable";
  sourceId: Uuid;
  reason: string;
}

/** Peak/RMS je Quelle, höchstens zehnmal pro Sekunde publiziert. */
export interface LevelsEvent {
  type: "levels";
  entries: Array<{ sourceId: Uuid; peak: number; rms: number }>;
}

/** Konkreter Hotkey-Konflikt; bestehende Bindung bleibt aktiv. */
export interface HotkeyErrorEvent {
  type: "hotkey_error";
  sceneId: Uuid;
  message: string;
}

export type DeviceRecoveryPhase = "started" | "succeeded" | "failed";

/** GPU-Verlust/Wiederherstellung; `detail` enthält bei Fehlern das HRESULT. */
export interface DeviceRecoveryEvent {
  type: "device_recovery";
  phase: DeviceRecoveryPhase;
  detail: string | null;
}

export interface MediaRuntimeState {
  playing: boolean;
  positionSeconds: number;
  durationSeconds: number | null;
}

export interface MediaStateEvent {
  type: "media_state";
  sourceId: Uuid;
  state: MediaRuntimeState;
}

export interface UnsupportedMediaEvent {
  type: "unsupported_media";
  sourceId: Uuid;
  reason: string;
}

export type AudioWarningKind = "underrun" | "overrun" | "device_invalidated";

export interface AudioWarningEvent {
  type: "audio_warning";
  kind: AudioWarningKind;
  message: string;
}

/** Unerwarteter Engine-Fehler, der keinem Quell-/Geräteereignis zugeordnet ist. */
export interface EngineErrorEvent {
  type: "engine_error";
  message: string;
}

export type EngineEvent =
  | SnapshotEvent
  | SourceAvailableEvent
  | SourceUnavailableEvent
  | LevelsEvent
  | HotkeyErrorEvent
  | DeviceRecoveryEvent
  | MediaStateEvent
  | UnsupportedMediaEvent
  | AudioWarningEvent
  | EngineErrorEvent;

export const ENGINE_EVENT_TYPES = [
  "snapshot",
  "source_available",
  "source_unavailable",
  "levels",
  "hotkey_error",
  "device_recovery",
  "media_state",
  "unsupported_media",
  "audio_warning",
  "engine_error",
] as const;

export type EngineEventType = (typeof ENGINE_EVENT_TYPES)[number];

/* ------------------------------------------------------------------ */
/* Ereignis-Wächter                                                    */
/* ------------------------------------------------------------------ */

import {
  isRecord,
  parseProjectV1,
} from "./project";

export function parseEngineEvent(value: unknown): EngineEvent | null {
  if (!isRecord(value)) return null;
  switch (value.type) {
    case "snapshot":
      return parseProjectV1(value.project)
        ? ({ type: "snapshot", project: parseProjectV1(value.project) as ProjectV1 })
        : null;
    case "source_available":
      return typeof value.sourceId === "string"
        ? { type: "source_available", sourceId: value.sourceId }
        : null;
    case "source_unavailable":
      return typeof value.sourceId === "string" && typeof value.reason === "string"
        ? { type: "source_unavailable", sourceId: value.sourceId, reason: value.reason }
        : null;
    case "levels": {
      if (!Array.isArray(value.entries)) return null;
      const entries: LevelsEvent["entries"] = [];
      for (const e of value.entries) {
        if (
          !isRecord(e) ||
          typeof e.sourceId !== "string" ||
          typeof e.peak !== "number" ||
          typeof e.rms !== "number"
        ) {
          return null;
        }
        entries.push({ sourceId: e.sourceId, peak: e.peak, rms: e.rms });
      }
      return { type: "levels", entries };
    }
    case "hotkey_error":
      return typeof value.sceneId === "string" && typeof value.message === "string"
        ? { type: "hotkey_error", sceneId: value.sceneId, message: value.message }
        : null;
    case "device_recovery": {
      const phase = value.phase;
      if (phase !== "started" && phase !== "succeeded" && phase !== "failed") return null;
      const detail = value.detail;
      if (detail !== null && typeof detail !== "string") return null;
      return { type: "device_recovery", phase, detail };
    }
    case "media_state": {
      if (typeof value.sourceId !== "string" || !isRecord(value.state)) return null;
      const s = value.state;
      if (typeof s.playing !== "boolean" || typeof s.positionSeconds !== "number") return null;
      if (s.durationSeconds !== null && typeof s.durationSeconds !== "number") return null;
      return {
        type: "media_state",
        sourceId: value.sourceId,
        state: {
          playing: s.playing,
          positionSeconds: s.positionSeconds,
          durationSeconds: s.durationSeconds,
        },
      };
    }
    case "unsupported_media":
      return typeof value.sourceId === "string" && typeof value.reason === "string"
        ? { type: "unsupported_media", sourceId: value.sourceId, reason: value.reason }
        : null;
    case "audio_warning": {
      const kind = value.kind;
      if (kind !== "underrun" && kind !== "overrun" && kind !== "device_invalidated") return null;
      if (typeof value.message !== "string") return null;
      return { type: "audio_warning", kind, message: value.message };
    }
    case "engine_error":
      return typeof value.message === "string" ? { type: "engine_error", message: value.message } : null;
    default:
      return null;
  }
}
