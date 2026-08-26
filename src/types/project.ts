/**
 * Persistierte Projektverträge – exakte Spiegelbilder der Rust-Typen aus
 * `hooviestar-engine/src/project.rs`.
 *
 * Konvention (serde): Tag-Feld `type`, Variantennamen snake_case,
 * Felder camelCase. Diese Datei beschreibt ausschließlich Zustand, den die
 * Engine autoritativ besitzt – das UI erfindet niemals persistierten Zustand.
 */

/** UUID v4 als String, wie von Rust `uuid::Uuid` serialisiert. */
export type Uuid = string;

/** sRGB-Hexfarbe inklusive `#`, z. B. `#101418`. */
export type HexColor = string;

export interface OutputConfig {
  width: number;
  height: number;
  fps: number;
  background: HexColor;
}

export interface Transform {
  /** Linke Kante in Ausgabepixeln. */
  x: number;
  /** Obere Kante in Ausgabepixeln. */
  y: number;
  width: number;
  height: number;
  /** Drehung in Grad, gegen den Uhrzeigersinn. */
  rotationDegrees: number;
  cropTop: number;
  cropRight: number;
  cropBottom: number;
  cropLeft: number;
  /** 0.0 ..= 1.0 */
  opacity: number;
}

/** Bindung einer Fensterquelle: kanonischer Prozesspfad + exakter Titel. */
export interface WindowBinding {
  processPath: string;
  windowTitle: string;
}

/** Bindung einer Monitorquelle: Adapter-LUID + Output-ID. */
export interface DisplayBinding {
  adapterLuid: string;
  outputId: number;
}

/** Bindung einer Audiositzung: Prozesspfad + Sitzungs-Gruppierungskennung. */
export interface AudioSessionBinding {
  processPath: string;
  sessionGroupingId: string;
}

interface SourceCommon {
  id: Uuid;
  name: string;
}

/** Fensterquelle (Windows Graphics Capture, Borderless/DWM). */
export interface WindowSource extends SourceCommon {
  type: "window";
  binding: WindowBinding;
}

/** Monitorquelle (Desktop Duplication, einziger Pfad für Exclusive Fullscreen). */
export interface DisplaySource extends SourceCommon {
  type: "display";
  binding: DisplayBinding;
}

/** Einmalig dekodierte Bilddatei als immutable GPU-Textur. */
export interface ImageSource extends SourceCommon {
  type: "image";
  /** Kanonischer absoluter Pfad. */
  path: string;
}

export type TextAlign = "left" | "center" | "right";

export interface TextSource extends SourceCommon {
  type: "text";
  text: string;
  fontFamily: string;
  fontSizePx: number;
  fontWeight: number;
  color: HexColor;
  backgroundColor: HexColor;
  align: TextAlign;
}

export interface MediaSource extends SourceCommon {
  type: "media";
  /** Kanonischer absoluter Pfad (H.264/AAC-MP4, MP3, WAV garantiert). */
  path: string;
  loop: boolean;
  continueWhenHidden: boolean;
  restartOnShow: boolean;
  /** 0.0 ..= 1.0 */
  volume: number;
  muted: boolean;
}

/** Nur-Audio-Quelle; wird gemischt, hat aber keinen sichtbaren Layer. */
export interface ApplicationAudioSource extends SourceCommon {
  type: "application_audio";
  binding: AudioSessionBinding;
  /** 0.0 ..= 1.0 */
  volume: number;
  muted: boolean;
}

export type Source =
  | WindowSource
  | DisplaySource
  | ImageSource
  | TextSource
  | MediaSource
  | ApplicationAudioSource;

export type SourceType = Source["type"];

export interface SceneItem {
  id: Uuid;
  sourceId: Uuid;
  visible: boolean;
  locked: boolean;
  transform: Transform;
}

export interface Scene {
  id: Uuid;
  name: string;
  /** Global registrierter Hotkey wie `Ctrl+Alt+1`; `null` = ungebunden. */
  hotkey: string | null;
  /**
   * Zeichenreihenfolge: früherer Eintrag unten, späterer Eintrag oben.
   */
  items: SceneItem[];
}

/** Einzige persistierte Projektwurzel (`version` ist literal `1`). */
export interface ProjectV1 {
  version: 1;
  output: OutputConfig;
  sources: Source[];
  scenes: Scene[];
  activeSceneId: Uuid;
}

/* ------------------------------------------------------------------ */
/* Laufzeit-Wächter (Validierung externer Daten, u. a. Fixtures)       */
/* ------------------------------------------------------------------ */

const SOURCE_TYPES: readonly SourceType[] = [
  "window",
  "display",
  "image",
  "text",
  "media",
  "application_audio",
];

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function str(value: unknown): value is string {
  return typeof value === "string";
}

function num(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function bool(value: unknown): value is boolean {
  return typeof value === "boolean";
}

function uuid(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function color(value: unknown): value is HexColor {
  return typeof value === "string" && /^#[0-9a-fA-F]{6}$/.test(value);
}

export function parseTransform(value: unknown): Transform | null {
  if (!isRecord(value)) return null;
  const keys = [
    "x",
    "y",
    "width",
    "height",
    "rotationDegrees",
    "cropTop",
    "cropRight",
    "cropBottom",
    "cropLeft",
    "opacity",
  ] as const;
  for (const k of keys) {
    if (!num(value[k])) return null;
  }
  const opacity = value.opacity as number;
  if (opacity < 0 || opacity > 1) return null;
  return {
    x: value.x as number,
    y: value.y as number,
    width: value.width as number,
    height: value.height as number,
    rotationDegrees: value.rotationDegrees as number,
    cropTop: value.cropTop as number,
    cropRight: value.cropRight as number,
    cropBottom: value.cropBottom as number,
    cropLeft: value.cropLeft as number,
    opacity,
  };
}

export function parseSource(value: unknown): Source | null {
  if (!isRecord(value)) return null;
  const type = value.type;
  if (!SOURCE_TYPES.includes(type as SourceType)) return null;
  if (!uuid(value.id) || !str(value.name)) return null;
  const id = value.id as string;
  const name = value.name as string;
  switch (type) {
    case "window": {
      const b = value.binding;
      if (!isRecord(b) || !str(b.processPath) || !str(b.windowTitle)) return null;
      return { type, id, name, binding: { processPath: b.processPath, windowTitle: b.windowTitle } };
    }
    case "display": {
      const b = value.binding;
      if (!isRecord(b) || !str(b.adapterLuid) || !num(b.outputId)) return null;
      return { type, id, name, binding: { adapterLuid: b.adapterLuid, outputId: b.outputId } };
    }
    case "image":
      return str(value.path) ? { type, id, name, path: value.path } : null;
    case "text": {
      if (
        !str(value.text) ||
        !str(value.fontFamily) ||
        !num(value.fontSizePx) ||
        !num(value.fontWeight) ||
        !color(value.color) ||
        !color(value.backgroundColor)
      ) {
        return null;
      }
      const align = value.align;
      if (align !== "left" && align !== "center" && align !== "right") return null;
      return {
        type,
        id,
        name,
        text: value.text,
        fontFamily: value.fontFamily,
        fontSizePx: value.fontSizePx,
        fontWeight: value.fontWeight,
        color: value.color,
        backgroundColor: value.backgroundColor,
        align,
      };
    }
    case "media": {
      if (
        !str(value.path) ||
        !bool(value.loop) ||
        !bool(value.continueWhenHidden) ||
        !bool(value.restartOnShow) ||
        !num(value.volume) || value.volume < 0 || value.volume > 1 ||
        !bool(value.muted)
      ) {
        return null;
      }
      return {
        type,
        id,
        name,
        path: value.path,
        loop: value.loop,
        continueWhenHidden: value.continueWhenHidden,
        restartOnShow: value.restartOnShow,
        volume: value.volume,
        muted: value.muted,
      };
    }
    case "application_audio": {
      const b = value.binding;
      if (!isRecord(b) || !str(b.processPath) || !str(b.sessionGroupingId)) return null;
      if (!num(value.volume) || value.volume < 0 || value.volume > 1 || !bool(value.muted)) return null;
      return {
        type,
        id,
        name,
        binding: { processPath: b.processPath, sessionGroupingId: b.sessionGroupingId },
        volume: value.volume,
        muted: value.muted,
      };
    }
  }
  return null;
}

export function parseSceneItem(value: unknown): SceneItem | null {
  if (!isRecord(value)) return null;
  const transform = parseTransform(value.transform);
  if (!uuid(value.id) || !uuid(value.sourceId) || !bool(value.visible) || !bool(value.locked) || !transform) {
    return null;
  }
  return {
    id: value.id as string,
    sourceId: value.sourceId as string,
    visible: value.visible as boolean,
    locked: value.locked as boolean,
    transform,
  };
}

export function parseScene(value: unknown): Scene | null {
  if (!isRecord(value) || !uuid(value.id) || !str(value.name)) return null;
  if (value.hotkey !== null && !str(value.hotkey)) return null;
  if (!Array.isArray(value.items)) return null;
  const items: SceneItem[] = [];
  for (const raw of value.items) {
    const item = parseSceneItem(raw);
    if (!item) return null;
    items.push(item);
  }
  return {
    id: value.id as string,
    name: value.name as string,
    hotkey: (value.hotkey ?? null) as string | null,
    items,
  };
}

export function parseOutputConfig(value: unknown): OutputConfig | null {
  if (!isRecord(value) || !num(value.width) || !num(value.height) || !num(value.fps) || !color(value.background)) {
    return null;
  }
  return {
    width: value.width as number,
    height: value.height as number,
    fps: value.fps as number,
    background: value.background as HexColor,
  };
}

/** Validiert ein vollständiges `ProjectV1` inklusive Referenzintegrität. */
export function parseProjectV1(value: unknown): ProjectV1 | null {
  if (!isRecord(value)) return null;
  if (value.version !== 1) return null;
  const output = parseOutputConfig(value.output);
  if (!output) return null;
  if (!Array.isArray(value.sources) || !Array.isArray(value.scenes)) return null;
  if (!uuid(value.activeSceneId)) return null;
  const sources: Source[] = [];
  const sourceIds = new Set<string>();
  for (const raw of value.sources) {
    const source = parseSource(raw);
    if (!source || sourceIds.has(source.id)) return null;
    sources.push(source);
    sourceIds.add(source.id);
  }
  const scenes: Scene[] = [];
  for (const raw of value.scenes) {
    const scene = parseScene(raw);
    if (!scene) return null;
    for (const item of scene.items) {
      if (!sourceIds.has(item.sourceId)) return null;
    }
    scenes.push(scene);
  }
  if (scenes.length === 0) return null;
  const activeSceneId = value.activeSceneId as string;
  if (!scenes.some((s) => s.id === activeSceneId)) return null;
  return { version: 1, output, sources, scenes, activeSceneId };
}

/** Bild-PiP rechts unten, wie vom Einrichtungsassistenten verwendet. */
export function pipTransform(output: OutputConfig): Transform {
  const width = Math.round(output.width * 0.35);
  const height = Math.round((width * 9) / 16);
  return {
    x: output.width - width - Math.round(output.width * 0.02),
    y: output.height - height - Math.round(output.height * 0.04),
    width,
    height,
    rotationDegrees: 0,
    cropTop: 0,
    cropRight: 0,
    cropBottom: 0,
    cropLeft: 0,
    opacity: 1,
  };
}
