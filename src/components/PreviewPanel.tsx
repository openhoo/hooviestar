import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent } from "react";
import type { OutputConfig, Scene, SceneItem, Source, Transform } from "../types";
import { isWindowsPlatform } from "../platform";

interface NativePointerPayload {
  phase: "down" | "move" | "up" | "cancel";
  pointerId: number;
  x: number;
  y: number;
  shiftKey: boolean;
  altKey: boolean;
  ctrlKey: boolean;
  button: number;
}

type ResizeHandle = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w";
type InteractionKind = "move" | "resize";

interface PreviewPanelProps {
  output: OutputConfig;
  activeSceneName: string;
  scene: Scene;
  sources: Source[];
  selectedSourceId: string | null;
  onSelectSource: (sourceId: string | null) => void;
  onTransform: (itemId: string, transform: Transform, expected?: Transform) => Promise<void> | void;
  onTransformError?: (message: string | null) => void;
  /** Dialogs hide both native surfaces so their HWND cannot occlude modal input. */
  nativeOverlayVisible?: boolean;
  onAttachBounds: (node: HTMLDivElement | null) => void;
}

interface OutputPoint {
  x: number;
  y: number;
}

interface Interaction {
  itemId: string;
  pointerId: number;
  kind: InteractionKind;
  handle: ResizeHandle | null;
  start: OutputPoint;
  initial: Transform;
}

interface DraftTransform {
  itemId: string;
  transform: Transform;
}

const MIN_SIZE = 1;
const HANDLE_HIT_SIZE = 14;

const HANDLE_NAMES: Record<ResizeHandle, string> = {
  nw: "oben links",
  n: "oben",
  ne: "oben rechts",
  e: "rechts",
  se: "unten rechts",
  s: "unten",
  sw: "unten links",
  w: "links",
};

function tauriRuntimeAvailable(): boolean {
  return typeof window !== "undefined"
    && typeof (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== "undefined";
}

function finitePoint(point: OutputPoint): boolean {
  return Number.isFinite(point.x) && Number.isFinite(point.y);
}

const TRANSFORM_EPSILON = 1e-3;

function sameTransform(left: Transform, right: Transform): boolean {
  return Math.abs(left.x - right.x) <= TRANSFORM_EPSILON
    && Math.abs(left.y - right.y) <= TRANSFORM_EPSILON
    && Math.abs(left.width - right.width) <= TRANSFORM_EPSILON
    && Math.abs(left.height - right.height) <= TRANSFORM_EPSILON
    && Math.abs(left.rotationDegrees - right.rotationDegrees) <= TRANSFORM_EPSILON
    && Math.abs(left.cropTop - right.cropTop) <= TRANSFORM_EPSILON
    && Math.abs(left.cropRight - right.cropRight) <= TRANSFORM_EPSILON
    && Math.abs(left.cropBottom - right.cropBottom) <= TRANSFORM_EPSILON
    && Math.abs(left.cropLeft - right.cropLeft) <= TRANSFORM_EPSILON
    && Math.abs(left.opacity - right.opacity) <= TRANSFORM_EPSILON;
}

function clampCrop(transform: Transform): Transform {
  const cropLeft = Math.min(Math.max(transform.cropLeft, 0), Math.max(0, transform.width - MIN_SIZE));
  const cropRight = Math.min(
    Math.max(transform.cropRight, 0),
    Math.max(0, transform.width - cropLeft - MIN_SIZE),
  );
  const cropTop = Math.min(Math.max(transform.cropTop, 0), Math.max(0, transform.height - MIN_SIZE));
  const cropBottom = Math.min(
    Math.max(transform.cropBottom, 0),
    Math.max(0, transform.height - cropTop - MIN_SIZE),
  );
  return { ...transform, cropLeft, cropRight, cropTop, cropBottom };
}

function rotateDelta(delta: OutputPoint, degrees: number): OutputPoint {
  const radians = degrees * Math.PI / 180;
  const cosine = Math.cos(radians);
  const sine = Math.sin(radians);
  return {
    x: cosine * delta.x + sine * delta.y,
    y: -sine * delta.x + cosine * delta.y,
  };
}

function resizeTransform(initial: Transform, handle: ResizeHandle, delta: OutputPoint): Transform {
  const local = rotateDelta(delta, initial.rotationDegrees);
  const horizontal = handle.includes("w") ? -1 : handle.includes("e") ? 1 : 0;
  const vertical = handle.includes("n") ? -1 : handle.includes("s") ? 1 : 0;
  const width = Math.max(MIN_SIZE, initial.width + horizontal * local.x);
  const height = Math.max(MIN_SIZE, initial.height + vertical * local.y);
  // Move the center in rotated output space, keeping the opposite handle fixed.
  const centerShift = rotateDelta({
    x: horizontal * (width - initial.width) / 2,
    y: vertical * (height - initial.height) / 2,
  }, -initial.rotationDegrees);
  return clampCrop({
    ...initial,
    x: initial.x + (initial.width - width) / 2 + centerShift.x,
    y: initial.y + (initial.height - height) / 2 + centerShift.y,
    width,
    height,
  });
}

function resizeByKeyboard(initial: Transform, delta: OutputPoint): Transform {
  return clampCrop({
    ...initial,
    width: Math.max(MIN_SIZE, initial.width + delta.x),
    height: Math.max(MIN_SIZE, initial.height + delta.y),
  });
}

function moveTransform(initial: Transform, delta: OutputPoint): Transform {
  return { ...initial, x: initial.x + delta.x, y: initial.y + delta.y };
}

function pointToLocal(point: OutputPoint, transform: Transform): OutputPoint {
  const center = {
    x: transform.x + transform.width * 0.5,
    y: transform.y + transform.height * 0.5,
  };
  const local = rotateDelta(
    { x: point.x - center.x, y: point.y - center.y },
    transform.rotationDegrees,
  );
  return {
    x: local.x + transform.width * 0.5,
    y: local.y + transform.height * 0.5,
  };
}

function handleAtPoint(
  point: OutputPoint,
  transform: Transform,
  hitSize = HANDLE_HIT_SIZE,
): ResizeHandle | null {
  const local = pointToLocal(point, transform);
  const nearLeft = Math.abs(local.x) <= hitSize;
  const nearRight = Math.abs(local.x - transform.width) <= hitSize;
  const nearTop = Math.abs(local.y) <= hitSize;
  const nearBottom = Math.abs(local.y - transform.height) <= hitSize;
  const withinHorizontalEdge = local.x >= -hitSize && local.x <= transform.width + hitSize;
  const withinVerticalEdge = local.y >= -hitSize && local.y <= transform.height + hitSize;
  if (nearTop && nearLeft) return "nw";
  if (nearTop && nearRight) return "ne";
  if (nearBottom && nearRight) return "se";
  if (nearBottom && nearLeft) return "sw";
  if (nearTop && withinHorizontalEdge) return "n";
  if (nearRight && withinVerticalEdge) return "e";
  if (nearBottom && withinHorizontalEdge) return "s";
  if (nearLeft && withinVerticalEdge) return "w";
  return null;
}

function itemContainsPoint(point: OutputPoint, transform: Transform): boolean {
  const local = pointToLocal(point, transform);
  return local.x >= 0 && local.x <= transform.width && local.y >= 0 && local.y <= transform.height;
}

function isEditableTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLElement
    && Boolean(target.closest("input, textarea, select, button, [contenteditable='true'], [role='dialog']"));
}

function sourceIsVisual(source: Source | undefined): boolean {
  return Boolean(source && source.type !== "application_audio");
}

function PreviewPanelImpl({
  output,
  activeSceneName,
  scene,
  sources,
  selectedSourceId,
  onSelectSource,
  onTransform,
  onTransformError,
  nativeOverlayVisible = true,
  onAttachBounds,
}: PreviewPanelProps) {
  const frameRef = useRef<HTMLDivElement | null>(null);
  const sceneRef = useRef(scene);
  const outputRef = useRef(output);
  const sourcesRef = useRef(sources);
  const selectedSourceIdRef = useRef(selectedSourceId);
  const draftRef = useRef<DraftTransform | null>(null);
  const pendingTransformRef = useRef<DraftTransform | null>(null);
  const interactionRef = useRef<Interaction | null>(null);
  const keyboardBaseRef = useRef<DraftTransform | null>(null);
  const onSelectSourceRef = useRef(onSelectSource);
  const onTransformRef = useRef(onTransform);
  const onTransformErrorRef = useRef(onTransformError);
  const [draft, setDraft] = useState<DraftTransform | null>(null);
  const [hoveredItemId, setHoveredItemId] = useState<string | null>(null);
  const [interactionError, setInteractionError] = useState<string | null>(null);

  useEffect(() => {
    sceneRef.current = scene;
    outputRef.current = output;
    sourcesRef.current = sources;
    selectedSourceIdRef.current = selectedSourceId;
    onSelectSourceRef.current = onSelectSource;
    onTransformRef.current = onTransform;
    onTransformErrorRef.current = onTransformError;
  }, [scene, output, sources, selectedSourceId, onSelectSource, onTransform, onTransformError]);

  const visualItems = useMemo(
    () => scene.items.filter((item) => item.visible && sourceIsVisual(sources.find((source) => source.id === item.sourceId))),
    [scene.items, sources],
  );
  const selectedItem = scene.items.find((item) => item.sourceId === selectedSourceId) ?? null;
  useEffect(() => {
    const current = draftRef.current;
    if (current && !scene.items.some((item) => item.id === current.itemId)) {
      draftRef.current = null;
      setDraft(null);
    }
    if (interactionRef.current && !scene.items.some((item) => item.id === interactionRef.current?.itemId)) {
      interactionRef.current = null;
    }
    // A pointer-down may select a different source and start a drag in the
    // same event. Never clear that live interaction just because selection
    // changed; selection changes outside an interaction reset keyboard undo.
    if (!interactionRef.current) keyboardBaseRef.current = null;
  }, [scene.id, selectedSourceId]);

  useEffect(() => {
    const active = interactionRef.current;
    const activeItem = active
      ? scene.items.find((entry) => entry.id === active.itemId)
      : null;
    if (active && (!activeItem || !sameTransform(activeItem.transform, active.initial))) {
      interactionRef.current = null;
      draftRef.current = null;
      setDraft(null);
      keyboardBaseRef.current = null;
      setInteractionError(null);
      onTransformErrorRef.current?.(null);
    }

    const pending = pendingTransformRef.current;
    const pendingItem = pending
      ? scene.items.find((entry) => entry.id === pending.itemId)
      : null;
    if (pending && (!pendingItem || sameTransform(pendingItem.transform, pending.transform))) {
      pendingTransformRef.current = null;
      keyboardBaseRef.current = null;
      if (!interactionRef.current && draftRef.current?.itemId === pending.itemId) {
        draftRef.current = null;
        setDraft(null);
      }
    }
    const current = draftRef.current;
    if (!current) return;
    const item = scene.items.find((entry) => entry.id === current.itemId);
    if (!item) return;
    if (!interactionRef.current && !pendingTransformRef.current && !sameTransform(item.transform, current.transform)) {
      draftRef.current = null;
      setDraft(null);
      keyboardBaseRef.current = null;
    }
  }, [scene.id, scene.items]);

  const setDraftValue = useCallback((next: DraftTransform | null) => {
    draftRef.current = next;
    setDraft(next);
  }, []);

  const displayTransform = useCallback((item: SceneItem): Transform => (
    draftRef.current?.itemId === item.id
      ? draftRef.current.transform
      : pendingTransformRef.current?.itemId === item.id
        ? pendingTransformRef.current.transform
        : item.transform
  ), []);

  const reportError = useCallback((error: unknown) => {
    const message = String(error);
    setInteractionError(message);
    onTransformErrorRef.current?.(message);
  }, []);

  const clearError = useCallback(() => {
    setInteractionError(null);
    onTransformErrorRef.current?.(null);
  }, []);

  const commitTransform = useCallback((itemId: string, transform: Transform, expected?: Transform) => {
    pendingTransformRef.current = { itemId, transform };
    const handleError = (error: unknown) => {
      const pending = pendingTransformRef.current;
      if (pending?.itemId === itemId && sameTransform(pending.transform, transform)) {
        pendingTransformRef.current = null;
      }
      if (draftRef.current?.itemId === itemId && sameTransform(draftRef.current.transform, transform)) {
        setDraftValue(null);
      }
      reportError(error);
    };
    try {
      void Promise.resolve(onTransformRef.current(itemId, transform, expected)).catch(handleError);
    } catch (error) {
      handleError(error);
    }
  }, [reportError, setDraftValue]);

  const pointFromClient = useCallback((clientX: number, clientY: number): OutputPoint | null => {
    const frame = frameRef.current;
    const rect = frame?.getBoundingClientRect();
    if (!rect || rect.width <= 0 || rect.height <= 0) return null;
    const point = {
      x: (clientX - rect.left) * outputRef.current.width / rect.width,
      y: (clientY - rect.top) * outputRef.current.height / rect.height,
    };
    return finitePoint(point) ? point : null;
  }, []);

  const handleHitSize = useCallback(() => {
    const rect = frameRef.current?.getBoundingClientRect();
    const outputWidth = outputRef.current.width;
    const outputHeight = outputRef.current.height;
    if (
      !rect
      || rect.width <= 0
      || rect.height <= 0
      || outputWidth <= 0
      || outputHeight <= 0
    ) return HANDLE_HIT_SIZE;
    const scale = Math.min(rect.width / outputWidth, rect.height / outputHeight);
    return Number.isFinite(scale) && scale > 0
      ? Math.max(HANDLE_HIT_SIZE, 12 / scale)
      : HANDLE_HIT_SIZE;
  }, []);

  const itemAtPoint = useCallback((point: OutputPoint): SceneItem | null => {
    const currentScene = sceneRef.current;
    const currentSources = sourcesRef.current;
    for (const item of [...currentScene.items].reverse()) {
      if (!item.visible || !sourceIsVisual(currentSources.find((source) => source.id === item.sourceId))) continue;
      if (itemContainsPoint(point, displayTransform(item))) return item;
    }
    return null;
  }, [displayTransform]);

  const finishInteraction = useCallback((cancelled: boolean, pointerId?: number) => {
    const interaction = interactionRef.current;
    if (!interaction || (pointerId !== undefined && interaction.pointerId !== pointerId)) return;
    interactionRef.current = null;
    const current = draftRef.current;
    const changed = current?.itemId === interaction.itemId
      && !sameTransform(current.transform, interaction.initial);
    if (cancelled || !changed || !current) {
      setDraftValue(null);
      return;
    }
    setDraftValue(null);
    clearError();
    keyboardBaseRef.current = null;
    commitTransform(interaction.itemId, current.transform, interaction.initial);
  }, [clearError, commitTransform, setDraftValue]);

  const beginInteraction = useCallback((
    item: SceneItem,
    point: OutputPoint,
    pointerId: number,
    handle: ResizeHandle | null,
  ) => {
    const initial = displayTransform(item);
    onSelectSourceRef.current(item.sourceId);
    clearError();
    if (item.locked) return;
    keyboardBaseRef.current = null;
    interactionRef.current = {
      itemId: item.id,
      pointerId,
      kind: handle ? "resize" : "move",
      handle,
      start: point,
      initial,
    };
  }, [clearError, displayTransform]);

  const moveInteraction = useCallback((point: OutputPoint, pointerId: number) => {
    const interaction = interactionRef.current;
    if (!interaction || interaction.pointerId !== pointerId || !finitePoint(point)) return;
    const delta = { x: point.x - interaction.start.x, y: point.y - interaction.start.y };
    const transform = interaction.kind === "move"
      ? moveTransform(interaction.initial, delta)
      : resizeTransform(interaction.initial, interaction.handle!, delta);
    if (sameTransform(transform, interaction.initial)) {
      setDraftValue(null);
      return;
    }
    setDraftValue({ itemId: interaction.itemId, transform });
  }, [setDraftValue]);

  const handlePointerDown = useCallback((event: ReactPointerEvent<HTMLElement>) => {
    if (event.button !== 0) return;
    const point = pointFromClient(event.clientX, event.clientY);
    if (!point) return;
    const itemElement = event.target instanceof Element ? event.target.closest<HTMLElement>("[data-preview-item]") : null;
    const itemId = itemElement?.dataset.previewItem;
    const item = itemId
      ? sceneRef.current.items.find((entry) => entry.id === itemId) ?? null
      : itemAtPoint(point);
    if (!item) {
      onSelectSourceRef.current(null);
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    frameRef.current?.focus({ preventScroll: true });
    const handle = itemElement?.dataset.resize as ResizeHandle | undefined;
    beginInteraction(item, point, event.pointerId, handle ?? null);
    try {
      frameRef.current?.setPointerCapture(event.pointerId);
    } catch {
      // Pointer capture is unavailable in jsdom and optional on embedded WebViews.
    }
  }, [beginInteraction, itemAtPoint, pointFromClient]);
  const handlePointerMove = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    const point = pointFromClient(event.clientX, event.clientY);
    if (point) moveInteraction(point, event.pointerId);
  }, [moveInteraction, pointFromClient]);

  const handlePointerUp = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    const point = pointFromClient(event.clientX, event.clientY);
    if (point) moveInteraction(point, event.pointerId);
    finishInteraction(false, event.pointerId);
    try {
      frameRef.current?.releasePointerCapture(event.pointerId);
    } catch {
      // Pointer capture is unavailable in jsdom and optional on embedded WebViews.
    }
  }, [finishInteraction, moveInteraction, pointFromClient]);

  const handlePointerCancel = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    finishInteraction(true, event.pointerId);
  }, [finishInteraction]);

  const handleLostPointerCapture = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    finishInteraction(true, event.pointerId);
  }, [finishInteraction]);

  const processNativePointer = useCallback((payload: NativePointerPayload) => {
    if (!Number.isFinite(payload.x) || !Number.isFinite(payload.y)) return;
    const point = { x: payload.x, y: payload.y };
    if (payload.phase === "down") {
      frameRef.current?.focus({ preventScroll: true });
      const selected = sceneRef.current.items.find((item) =>
        item.sourceId === selectedSourceIdRef.current
        && item.visible
        && sourceIsVisual(sourcesRef.current.find((source) => source.id === item.sourceId)),
      );
      const selectedHandle = selected ? handleAtPoint(point, displayTransform(selected), handleHitSize()) : null;
      const item = selectedHandle ? selected : itemAtPoint(point);
      if (!item) {
        onSelectSourceRef.current(null);
        return;
      }
      const handle = item === selected ? selectedHandle : null;
      beginInteraction(item, point, payload.pointerId, handle);
      return;
    }
    if (payload.phase === "move") {
      moveInteraction(point, payload.pointerId);
    } else if (payload.phase === "up") {
      moveInteraction(point, payload.pointerId);
      finishInteraction(false, payload.pointerId);
    } else if (payload.phase === "cancel") {
      finishInteraction(true, payload.pointerId);
    }
  }, [beginInteraction, displayTransform, finishInteraction, handleHitSize, itemAtPoint, moveInteraction]);

  useEffect(() => {
    if (!isWindowsPlatform() || !tauriRuntimeAvailable()) return;
    let active = true;
    let detach: (() => void) | null = null;
    const subscription = listen<NativePointerPayload>("preview-pointer", ({ payload }) => {
      if (active) processNativePointer(payload);
    });
    void subscription.then((unlisten) => {
      if (active) detach = unlisten;
      else unlisten();
    }, reportError);
    return () => {
      active = false;
      detach?.();
    };
  }, [processNativePointer, reportError]);

  const overlayTransform = selectedItem
    ? (draft?.itemId === selectedItem.id ? draft.transform : selectedItem.transform)
    : null;
  useEffect(() => {
    if (!isWindowsPlatform() || !tauriRuntimeAvailable()) return;
    const selection = selectedItem
      && selectedItem.visible
      && overlayTransform
      && sourceIsVisual(sources.find((source) => source.id === selectedItem.sourceId))
      ? { transform: overlayTransform, locked: selectedItem.locked }
      : null;
    void invoke("set_preview_overlay", {
      visible: nativeOverlayVisible,
      outputWidth: output.width,
      outputHeight: output.height,
      selection,
    }).catch(reportError);
  }, [
    nativeOverlayVisible,
    output.height,
    output.width,
    overlayTransform,
    reportError,
    selectedItem,
    sources,
  ]);

  useEffect(() => {
    const cancelOnWindowBlur = () => finishInteraction(true);
    window.addEventListener("blur", cancelOnWindowBlur);
    return () => window.removeEventListener("blur", cancelOnWindowBlur);
  }, [finishInteraction]);

  const handleKeyDown = useCallback((event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (isEditableTarget(event.target)) return;
    const selected = selectedItem
      ?? (selectedSourceIdRef.current
        ? sceneRef.current.items.find((item) => item.sourceId === selectedSourceIdRef.current) ?? null
        : null);
    if (event.key === "Escape") {
      const interaction = interactionRef.current;
      if (interaction) {
        event.preventDefault();
        finishInteraction(true);
        return;
      }
      const base = keyboardBaseRef.current;
      if (base) {
        event.preventDefault();
        keyboardBaseRef.current = null;
        const current = draftRef.current;
        setDraftValue(null);
        if (current && !sameTransform(current.transform, base.transform)) {
          commitTransform(base.itemId, base.transform, current.transform);
        }
      }
      return;
    }
    if (!selected || selected.locked || event.ctrlKey || event.metaKey) return;
    const movement = event.key === "ArrowLeft"
      ? { x: -1, y: 0 }
      : event.key === "ArrowRight"
        ? { x: 1, y: 0 }
        : event.key === "ArrowUp"
          ? { x: 0, y: -1 }
          : event.key === "ArrowDown"
            ? { x: 0, y: 1 }
            : null;
    if (!movement) return;
    event.preventDefault();
    const step = event.shiftKey ? 10 : 1;
    const initial = displayTransform(selected);
    if (!keyboardBaseRef.current || keyboardBaseRef.current.itemId !== selected.id) {
      keyboardBaseRef.current = { itemId: selected.id, transform: initial };
    }
    const delta = { x: movement.x * step, y: movement.y * step };
    const next = event.altKey
      ? resizeByKeyboard(initial, delta)
      : moveTransform(initial, delta);
    if (sameTransform(next, initial)) return;
    setDraftValue({ itemId: selected.id, transform: next });
    clearError();
    commitTransform(selected.id, next, initial);
  }, [clearError, commitTransform, displayTransform, finishInteraction, selectedItem, setDraftValue]);

  const setFrameRef = useCallback((node: HTMLDivElement | null) => {
    frameRef.current = node;
    onAttachBounds(node);
  }, [onAttachBounds]);

  return (
    <section className="preview-stage">
      <div
        id="native-preview-bounds"
        ref={setFrameRef}
        className="preview-frame"
        role="application"
        tabIndex={0}
        style={{
          aspectRatio: `${output.width} / ${output.height}`,
          backgroundColor: output.background,
        }}
        aria-label="Native Szenenvorschau"
        onKeyDown={handleKeyDown}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerCancel}
        onLostPointerCapture={handleLostPointerCapture}
      >
        <div className="preview-placeholder" aria-hidden="true">
          <strong>{activeSceneName}</strong>
          <span>{isWindowsPlatform() ? "Native D3D11-Vorschau" : "Vulkan-Ausgabe läuft im Hintergrund"}</span>
        </div>
        <div className="preview-items" aria-label="Szenenquellen">
          {visualItems.map((item) => {
            const source = sources.find((entry) => entry.id === item.sourceId);
            const transform = displayTransform(item);
            const selected = item.sourceId === selectedSourceId;
            const sourceName = source?.name ?? "Quelle";
            const style = {
              left: `${((transform.x + transform.width * 0.5) / output.width) * 100}%`,
              top: `${((transform.y + transform.height * 0.5) / output.height) * 100}%`,
              width: `${(transform.width / output.width) * 100}%`,
              height: `${(transform.height / output.height) * 100}%`,
              transform: `translate(-50%, -50%) rotate(${transform.rotationDegrees}deg)`,
              opacity: Math.max(0.05, Math.min(1, transform.opacity)),
            };
            return (
              <div
                key={item.id}
                className={[
                  "preview-item",
                  selected ? "selected" : "",
                  item.locked ? "locked" : "",
                  hoveredItemId === item.id ? "hovered" : "",
                ].filter(Boolean).join(" ")}
                data-preview-item={item.id}
                style={style}
                role="button"
                tabIndex={selected ? 0 : -1}
                aria-label={`${sourceName}${item.locked ? " (gesperrt)" : ""}`}
                aria-pressed={selected}
                onFocus={() => onSelectSourceRef.current(item.sourceId)}
                onMouseEnter={() => setHoveredItemId(item.id)}
                onMouseLeave={() => setHoveredItemId((current) => current === item.id ? null : current)}
              >
                {selected && (
                  <span className="preview-selection-label" aria-hidden="true">
                    {sourceName}{item.locked ? " · gesperrt" : ""}
                  </span>
                )}
                {selected && !item.locked && (Object.keys(HANDLE_NAMES) as ResizeHandle[]).map((handle) => (
                  <button
                    key={handle}
                    type="button"
                    className={`preview-handle preview-handle-${handle}`}
                    tabIndex={-1}
                    data-preview-item={item.id}
                    data-resize={handle}
                    aria-label={`${sourceName} Größe ${HANDLE_NAMES[handle]}`}
                    onPointerDown={(event) => {
                      event.stopPropagation();
                      handlePointerDown(event);
                    }}
                  />
                ))}
              </div>
            );
          })}
        </div>
        {interactionError && <p className="preview-error" role="alert">{interactionError}</p>}
        <p className="preview-key-help">
          Pfeile: Position · Shift + Pfeile: 10 px · Alt + Pfeile: Größe · Esc: Abbrechen
        </p>
      </div>
    </section>
  );
}

export const PreviewPanel = memo(PreviewPanelImpl);
