import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent } from "react";
import type { OutputConfig, Scene, SceneItem, Source, Transform } from "../types";
import { isWindowsPlatform } from "../platform";

interface NativePointerPayload {
  phase: "down" | "move" | "up" | "cancel" | "leave";
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
  dragging: boolean;
}

interface DraftTransform {
  itemId: string;
  transform: Transform;
}

const MIN_SIZE = 1;
/** Half of the 24 CSS-pixel handle hit target. */
const HANDLE_HIT_SIZE = 12;
const DRAG_THRESHOLD_CSS = 3;

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
function resizeCursorFor(handle: ResizeHandle, rotationDegrees: number): CSSProperties["cursor"] {
  const localAngle = handle.length === 2
    ? (handle === "nw" || handle === "se" ? 45 : 135)
    : (handle === "n" || handle === "s" ? 90 : 0);
  const normalized = ((localAngle + rotationDegrees) % 180 + 180) % 180;
  const axis = Math.round(normalized / 45) % 4;
  return ["ew-resize", "nwse-resize", "ns-resize", "nesw-resize"][axis] as CSSProperties["cursor"];
}

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
function formatPixel(value: number): string {
  if (!Number.isFinite(value)) return "—";
  const rounded = Math.round(value * 100) / 100;
  return String(rounded);
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
  const [, setDraft] = useState<DraftTransform | null>(null);
  const [hoveredItemId, setHoveredItemId] = useState<string | null>(null);
  const [interactingItemId, setInteractingItemId] = useState<string | null>(null);
  const [interactionError, setInteractionError] = useState<string | null>(null);

  const releasePointerCapture = useCallback((pointerId: number) => {
    try {
      frameRef.current?.releasePointerCapture?.(pointerId);
    } catch {
      // Pointer capture is unavailable in jsdom and optional on embedded WebViews.
    }
  }, []);

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
  const selectedVisualItem = selectedItem
    && selectedItem.visible
    && sourceIsVisual(sources.find((source) => source.id === selectedItem.sourceId))
    ? selectedItem
    : null;

  useEffect(() => {
    const current = draftRef.current;
    if (current && !scene.items.some((item) => item.id === current.itemId)) {
      draftRef.current = null;
      setDraft(null);
    }
    const active = interactionRef.current;
    if (active && !scene.items.some((item) => item.id === active.itemId)) {
      interactionRef.current = null;
      releasePointerCapture(active.pointerId);
      setInteractingItemId(null);
    }
    // A pointer-down may select a different source and start a drag in the
    // same event. Never clear that live interaction just because selection
    // changed; selection changes outside an interaction reset keyboard undo.
    if (!interactionRef.current) keyboardBaseRef.current = null;
  }, [releasePointerCapture, scene.id, selectedSourceId]);

  useEffect(() => {
    const active = interactionRef.current;
    const activeItem = active
      ? scene.items.find((entry) => entry.id === active.itemId)
      : null;
    if (active && (!activeItem || !sameTransform(activeItem.transform, active.initial))) {
      interactionRef.current = null;
      releasePointerCapture(active.pointerId);
      setInteractingItemId(null);
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
  }, [releasePointerCapture, scene.id, scene.items]);

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
      ? HANDLE_HIT_SIZE / scale
      : HANDLE_HIT_SIZE;
  }, []);

  const dragThreshold = useCallback(() => {
    const rect = frameRef.current?.getBoundingClientRect();
    const outputWidth = outputRef.current.width;
    const outputHeight = outputRef.current.height;
    if (
      !rect
      || rect.width <= 0
      || rect.height <= 0
      || outputWidth <= 0
      || outputHeight <= 0
    ) return DRAG_THRESHOLD_CSS;
    const scale = Math.min(rect.width / outputWidth, rect.height / outputHeight);
    return Number.isFinite(scale) && scale > 0
      ? DRAG_THRESHOLD_CSS / scale
      : DRAG_THRESHOLD_CSS;
  }, []);

  const itemAtPoint = useCallback((point: OutputPoint): SceneItem | null => {
    const currentScene = sceneRef.current;
    const currentSources = sourcesRef.current;
    for (let index = currentScene.items.length - 1; index >= 0; index -= 1) {
      const item = currentScene.items[index];
      if (!item || !item.visible || !sourceIsVisual(currentSources.find((source) => source.id === item.sourceId))) continue;
      if (itemContainsPoint(point, displayTransform(item))) return item;
    }
    return null;
  }, [displayTransform]);

  const itemForHover = useCallback((point: OutputPoint): SceneItem | null => {
    const item = itemAtPoint(point);
    return item && item.sourceId !== selectedSourceIdRef.current ? item : null;
  }, [itemAtPoint]);

  const updateHover = useCallback((point: OutputPoint | null) => {
    setHoveredItemId(point ? itemForHover(point)?.id ?? null : null);
  }, [itemForHover]);

  const finishInteraction = useCallback((cancelled: boolean, pointerId?: number) => {
    const interaction = interactionRef.current;
    if (!interaction || (pointerId !== undefined && interaction.pointerId !== pointerId)) return;
    interactionRef.current = null;
    setInteractingItemId(null);
    releasePointerCapture(interaction.pointerId);
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
  }, [clearError, commitTransform, releasePointerCapture, setDraftValue]);

  const beginInteraction = useCallback((
    item: SceneItem,
    point: OutputPoint,
    pointerId: number,
    handle: ResizeHandle | null,
  ): boolean => {
    if (interactionRef.current) return false;
    const initial = displayTransform(item);
    onSelectSourceRef.current(item.sourceId);
    clearError();
    setHoveredItemId(null);
    if (item.locked) return false;
    keyboardBaseRef.current = null;
    interactionRef.current = {
      itemId: item.id,
      pointerId,
      kind: handle ? "resize" : "move",
      handle,
      start: point,
      initial,
      dragging: false,
    };
    setInteractingItemId(item.id);
    return true;
  }, [clearError, displayTransform]);

  const moveInteraction = useCallback((point: OutputPoint, pointerId: number) => {
    const interaction = interactionRef.current;
    if (!interaction || interaction.pointerId !== pointerId || !finitePoint(point)) return;
    const delta = { x: point.x - interaction.start.x, y: point.y - interaction.start.y };
    if (!interaction.dragging) {
      if (Math.hypot(delta.x, delta.y) < dragThreshold()) return;
      interaction.dragging = true;
    }
    const transform = interaction.kind === "move"
      ? moveTransform(interaction.initial, delta)
      : resizeTransform(interaction.initial, interaction.handle!, delta);
    if (sameTransform(transform, interaction.initial)) {
      setDraftValue(null);
      return;
    }
    setDraftValue({ itemId: interaction.itemId, transform });
  }, [dragThreshold, setDraftValue]);

  const handlePointerDown = useCallback((event: ReactPointerEvent<HTMLElement>) => {
    if (event.button !== 0 || interactionRef.current) return;
    const point = pointFromClient(event.clientX, event.clientY);
    if (!point) return;
    const itemElement = event.target instanceof Element ? event.target.closest<HTMLElement>("[data-preview-item]") : null;
    const itemId = itemElement?.dataset.previewItem;
    const currentSelected = sceneRef.current.items.find((item) =>
      item.sourceId === selectedSourceIdRef.current
      && item.visible
      && sourceIsVisual(sourcesRef.current.find((source) => source.id === item.sourceId)),
    );
    const selectedHandle = currentSelected && !currentSelected.locked
      ? handleAtPoint(point, displayTransform(currentSelected), handleHitSize())
      : null;
    const item = selectedHandle
      ? currentSelected
      : itemId
        ? sceneRef.current.items.find((entry) => entry.id === itemId) ?? null
        : itemAtPoint(point);
    if (!item) {
      onSelectSourceRef.current(null);
      setHoveredItemId(null);
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    frameRef.current?.focus({ preventScroll: true });
    const targetHandle = itemElement?.dataset.resize as ResizeHandle | undefined;
    const handle = selectedHandle ?? (item === currentSelected ? targetHandle ?? null : null);
    const started = beginInteraction(item, point, event.pointerId, handle);
    if (!started) return;
    try {
      frameRef.current?.setPointerCapture(event.pointerId);
    } catch {
      // Pointer capture is unavailable in jsdom and optional on embedded WebViews.
    }
  }, [beginInteraction, displayTransform, handleHitSize, itemAtPoint, pointFromClient]);

  const handlePointerMove = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    const point = pointFromClient(event.clientX, event.clientY);
    if (!point) return;
    if (interactionRef.current) {
      moveInteraction(point, event.pointerId);
    } else {
      updateHover(point);
    }
  }, [moveInteraction, pointFromClient, updateHover]);

  const handlePointerUp = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    const point = pointFromClient(event.clientX, event.clientY);
    const interaction = interactionRef.current;
    if (interaction?.pointerId === event.pointerId) {
      if (point) moveInteraction(point, event.pointerId);
      finishInteraction(false, event.pointerId);
      updateHover(point);
    } else if (!interaction && point) {
      updateHover(point);
    }
  }, [finishInteraction, moveInteraction, pointFromClient, updateHover]);

  const handlePointerCancel = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    const interaction = interactionRef.current;
    if (interaction?.pointerId !== event.pointerId) return;
    finishInteraction(true, event.pointerId);
    setHoveredItemId(null);
  }, [finishInteraction]);

  const handleLostPointerCapture = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    const interaction = interactionRef.current;
    if (interaction?.pointerId !== event.pointerId) return;
    finishInteraction(true, event.pointerId);
    setHoveredItemId(null);
  }, [finishInteraction]);

  const processNativePointer = useCallback((payload: NativePointerPayload) => {
    if (payload.phase === "leave") {
      if (!interactionRef.current) setHoveredItemId(null);
      return;
    }
    if (!Number.isFinite(payload.x) || !Number.isFinite(payload.y)) return;
    const point = { x: payload.x, y: payload.y };
    if (payload.phase === "down") {
      if (interactionRef.current) return;
      frameRef.current?.focus({ preventScroll: true });
      const selected = sceneRef.current.items.find((item) =>
        item.sourceId === selectedSourceIdRef.current
        && item.visible
        && sourceIsVisual(sourcesRef.current.find((source) => source.id === item.sourceId)),
      );
      const selectedHandle = selected && !selected.locked
        ? handleAtPoint(point, displayTransform(selected), handleHitSize())
        : null;
      const item = selectedHandle ? selected : itemAtPoint(point);
      if (!item) {
        onSelectSourceRef.current(null);
        setHoveredItemId(null);
        return;
      }
      const handle = item === selected ? selectedHandle : null;
      beginInteraction(item, point, payload.pointerId, handle);
      return;
    }
    const interaction = interactionRef.current;
    if (payload.phase === "move") {
      if (interaction) {
        moveInteraction(point, payload.pointerId);
      } else {
        updateHover(point);
      }
    } else if (payload.phase === "up") {
      if (interaction?.pointerId === payload.pointerId) {
        moveInteraction(point, payload.pointerId);
        finishInteraction(false, payload.pointerId);
        updateHover(point);
      } else if (!interaction) {
        updateHover(point);
      }
    } else if (payload.phase === "cancel") {
      if (!interaction || interaction.pointerId === payload.pointerId) {
        finishInteraction(true, payload.pointerId);
        setHoveredItemId(null);
      }
    }
  }, [
    beginInteraction,
    displayTransform,
    finishInteraction,
    handleHitSize,
    itemAtPoint,
    moveInteraction,
    updateHover,
  ]);

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

  const overlayTransform = selectedVisualItem ? displayTransform(selectedVisualItem) : null;
  const hoveredItem = !interactingItemId && hoveredItemId
    ? visualItems.find((item) => item.id === hoveredItemId) ?? null
    : null;
  const hoverTransform = hoveredItem ? displayTransform(hoveredItem) : null;
  useEffect(() => {
    if (!isWindowsPlatform() || !tauriRuntimeAvailable()) return;
    const selection = selectedVisualItem && overlayTransform
      ? { transform: overlayTransform, locked: selectedVisualItem.locked }
      : null;
    const hover = hoveredItem
      && hoverTransform
      && hoveredItem.sourceId !== selectedSourceIdRef.current
      ? { transform: hoverTransform, locked: hoveredItem.locked }
      : null;
    void invoke("set_preview_overlay", {
      visible: nativeOverlayVisible,
      outputWidth: output.width,
      outputHeight: output.height,
      selection,
      hover,
    }).catch(reportError);
  }, [
    hoverTransform,
    hoveredItem,
    nativeOverlayVisible,
    output.height,
    output.width,
    overlayTransform,
    reportError,
    selectedVisualItem,
  ]);

  useEffect(() => {
    const cancelOnWindowBlur = () => {
      finishInteraction(true);
      setHoveredItemId(null);
    };
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
    if (
      !selected
      || !selected.visible
      || selected.locked
      || !sourceIsVisual(sourcesRef.current.find((source) => source.id === selected.sourceId))
      || event.ctrlKey
      || event.metaKey
    ) return;
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

  const selectedSourceName = selectedVisualItem
    ? sources.find((source) => source.id === selectedVisualItem.sourceId)?.name ?? "Quelle"
    : null;
  const hasSelectionDetails = Boolean(selectedVisualItem && overlayTransform && selectedSourceName);
  const canvasStyle = { "--preview-aspect": String(output.width / output.height) } as CSSProperties;

  return (
    <section className="preview-stage">
      <div className="preview-canvas-area" style={canvasStyle}>
      <div
        id="native-preview-bounds"
        ref={setFrameRef}
        className={["preview-frame", interactingItemId ? "is-interacting" : ""].filter(Boolean).join(" ")}
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
            };
            return (
              <div
                key={item.id}
                className={[
                  "preview-item",
                  selected ? "selected" : "",
                  item.locked ? "locked" : "",
                  hoveredItemId === item.id ? "hovered" : "",
                  interactingItemId === item.id ? "is-interacting" : "",
                ].filter(Boolean).join(" ")}
                data-preview-item={item.id}
                style={style}
                role="button"
                tabIndex={selected ? 0 : -1}
                aria-label={`${sourceName}${item.locked ? " (gesperrt)" : ""}`}
                aria-pressed={selected}
                aria-disabled={item.locked}
                onFocus={() => onSelectSourceRef.current(item.sourceId)}
                onMouseEnter={() => {
                  if (!interactionRef.current && !selected) setHoveredItemId(item.id);
                }}
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
                    style={{ cursor: resizeCursorFor(handle, transform.rotationDegrees) }}
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
      </div>
      </div>
      <div
        className="preview-selection-details"
        data-empty={hasSelectionDetails ? "false" : "true"}
        aria-label="Auswahl Details"
        aria-live="polite"
        aria-hidden={!hasSelectionDetails}
      >
        <strong className="preview-selection-name">{selectedSourceName ?? "\u00a0"}</strong>
        <span className="preview-selection-state">
          {selectedVisualItem?.locked ? "Gesperrt" : "\u00a0"}
        </span>
        <dl className="preview-selection-geometry">
          <div><dt>X</dt><dd>{overlayTransform ? `${formatPixel(overlayTransform.x)} px` : "\u00a0"}</dd></div>
          <div><dt>Y</dt><dd>{overlayTransform ? `${formatPixel(overlayTransform.y)} px` : "\u00a0"}</dd></div>
          <div><dt>Breite</dt><dd>{overlayTransform ? `${formatPixel(overlayTransform.width)} px` : "\u00a0"}</dd></div>
          <div><dt>Höhe</dt><dd>{overlayTransform ? `${formatPixel(overlayTransform.height)} px` : "\u00a0"}</dd></div>
        </dl>
      </div>
      <p className="preview-editor-help preview-key-help">
        Ziehen: verschieben · Griffe: Größe ändern · Pfeile: 1 px · Shift + Pfeile: 10 px · Alt + Pfeile: Größe · Esc: Abbrechen
      </p>
    </section>
  );
}

export const PreviewPanel = memo(PreviewPanelImpl);
