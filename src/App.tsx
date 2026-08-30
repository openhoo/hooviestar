import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { engineStore } from "./engineStore";
import { pipTransform } from "./types";
import type { EngineCommand, ProjectV1, Scene, Source, SourceCandidate, TextSource, Transform } from "./types";
import { runGuarded } from "./guarded";
import { SerialQueue } from "./serialQueue";
import { useAudioFieldBridge } from "./hooks/useAudioFieldBridge";
import { isWindowsPlatform } from "./platform";
import { AddSourceDialog } from "./components/AddSourceDialog";
import { ScenesPanel } from "./components/ScenesPanel";
import { PreviewPanel } from "./components/PreviewPanel";
import { SourceInspectorPanel } from "./components/SourceInspectorPanel";
import type { ItemAction } from "./components/SourceInspectorPanel";
import { AudioMixerPanel } from "./components/AudioMixerPanel";
import { OnboardingBanner } from "./components/OnboardingBanner";
import { SourcesPanel } from "./components/SourcesPanel";
import type { SourceRow } from "./components/SourcesPanel";
import { ControlsDock } from "./components/ControlsDock";
import { StatusBar } from "./components/StatusBar";
import { updateStatusMessage } from "./updateStatus";
import type { UpdateStatusEvent } from "./updateStatus";


function fullTransform(width: number, height: number): Transform {
  return {
    x: 0,
    y: 0,
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

/**
 * Stabile Rollenzuordnung der Standard-Szenen. Die Seeds in
 * crates/hooviestar-engine/src/project.rs (`ProjectV1::empty`) erzeugen die
 * Szenen-UUIDs bei jedem Start neu (`Uuid::new_v4`), daher sind die Hotkeys
 * der einzige persistente Vertrag – Szenennamen dürfen sich jederzeit
 * umbenennen lassen, ohne diese Zuordnung zu brechen.
 */
const GAME_SCENE_HOTKEY = "Ctrl+Alt+1";
const VIDEO_SCENE_HOTKEY = "Ctrl+Alt+2";
const BOTH_SCENE_HOTKEY = "Ctrl+Alt+3";

interface SceneTarget { scene: Scene; transform: Transform; bottom?: boolean }

function activeSceneOf(project: ProjectV1): Scene {
  const scene = project.scenes.find((entry) => entry.id === project.activeSceneId);
  if (!scene) throw new Error("Aktive Szene fehlt im validierten Projekt");
  return scene;
}

export default function App() {
  const { project, status, levels, mediaStates } = useSyncExternalStore(
    engineStore.subscribe,
    engineStore.getSnapshot,
  );
  const [selectedSourceId, setSelectedSourceId] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const [onboarding, setOnboarding] = useState(false);
  const [hotkeyMessage, setHotkeyMessage] = useState<string | null>(null);
  const [sceneError, setSceneError] = useState<string | null>(null);
  const [itemError, setItemError] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [windowError, setWindowError] = useState<string | null>(null);
  const [updateStatus, setUpdateStatus] = useState<string | null>(null);
  const onboardingDismissedRef = useRef(false);
  // Optimistische Feld-Deltas je Quelle: überbrückt das Snapshot-Event-Lag, damit
  // aufeinanderfolgende update_source-Aufrufe nicht gegenseitig Felder zurücksetzen.
  // Lautstärke/Stumm gehen über eigene Engine-Befehle, werden aber ebenfalls hier
  // überlagert, damit ein parallel laufendes update_source sie nicht mit dem
  // Snapshot-Stand zurücküberschreibt.
  const pendingSourceFieldsRef = useRef(new Map<string, Record<string, unknown>>());
  // Alle persistierten Mutationen derselben Quelle teilen eine Reihenfolge.
  // Ohne diese Queue können ältere IPC-Aufrufe später abschließen und neuere
  // Text-/Lautstärke-/Stummwerte wieder überschreiben.
  const sourceMutationQueueRef = useRef(new SerialQueue());
  // Audio-Brücke: IPC-Koaleszierung für Lautstärke/Stumm besitzt die Hook;
  // das Feld-Overlay (pendingSourceFieldsRef) bleibt hier und wird übergeben.
  const {
    audioError,
    setAudioField,
    toggleMixerMute,
    setMixerVolume,
    pendingField,
    prunePendingFields,
  } = useAudioFieldBridge(pendingSourceFieldsRef, sourceMutationQueueRef.current);
  const [textError, setTextError] = useState<string | null>(null);
  const addButton = useRef<HTMLButtonElement>(null);
  // Native-Vorschau: das Element überlebt Projekt-Updates; Beobachter und
  // Meldung werden genau einmal eingerichtet (siehe attachPreviewBounds).
  const previewNodeRef = useRef<HTMLDivElement | null>(null);
  const previewObserverRef = useRef<ResizeObserver | null>(null);
  const previewRequestRef = useRef(0);

  useEffect(() => {
    void engineStore.start();
  }, []);
  useEffect(() => {
    let active = true;
    let clearTimer: ReturnType<typeof setTimeout> | null = null;
    let detachListener: (() => void) | null = null;
    const showStatus = (payload: UpdateStatusEvent) => {
      if (!active) return;
      if (clearTimer) clearTimeout(clearTimer);
      setUpdateStatus(updateStatusMessage(payload));
      if (payload.status === "up_to_date") {
        clearTimer = setTimeout(() => {
          if (active) setUpdateStatus(null);
        }, 5_000);
      }
    };
    const unlisten = listen<UpdateStatusEvent>("updater-status", ({ payload }) => {
      showStatus(payload);
    });
    void unlisten.then(async (detach) => {
      if (!active) {
        detach();
        return;
      }
      detachListener = detach;
      try {
        const current = await invoke<UpdateStatusEvent | null>("updater_status");
        if (current) showStatus(current);
      } catch (error) {
        if (active) setUpdateStatus(`Aktualisierungsstatus nicht verfügbar: ${String(error)}`);
      }
    });
    return () => {
      active = false;
      if (clearTimer) clearTimeout(clearTimer);
      detachListener?.();
    };
  }, []);
  useEffect(() => {
    if (!project) return;
    prunePendingFields(project);
    // Onboarding nur einmal pro Session automatisch öffnen; "Später" bleibt klebrig.
    if (!onboardingDismissedRef.current && project.sources.length === 0) setOnboarding(true);
  }, [project, prunePendingFields]);
  useEffect(() => () => {
    previewObserverRef.current?.disconnect();
    previewObserverRef.current = null;
  }, []);

  const reportPreviewBounds = useCallback(() => {
    const preview = previewNodeRef.current;
    if (!preview || !isWindowsPlatform()) return;
    const bounds = preview.getBoundingClientRect();
    const request = ++previewRequestRef.current;
    void invoke("set_preview_bounds", {
      x: bounds.x,
      y: bounds.y,
      width: bounds.width,
      height: bounds.height,
    }).then(
      () => {
        if (previewRequestRef.current === request) setPreviewError(null);
      },
      (error: unknown) => {
        if (previewRequestRef.current === request) {
          setPreviewError(`Vorschaufehler: ${String(error)}`);
        }
      },
    );
  }, []);

  // Ref-Callback statt Effect: das Element wird beim Einhängen beobachtet,
  // der Beobachter entsteht höchstens einmal und hängt nicht an `project`.
  const attachPreviewBounds = useCallback((node: HTMLDivElement | null) => {
    const previous = previewNodeRef.current;
    if (previous === node) return;
    if (previous) previewObserverRef.current?.unobserve(previous);
    previewNodeRef.current = node;
    if (!node || !isWindowsPlatform()) return;
    if (!previewObserverRef.current) {
      previewObserverRef.current = new ResizeObserver(() => reportPreviewBounds());
    }
    previewObserverRef.current.observe(node);
    reportPreviewBounds();
  }, [reportPreviewBounds]);

  const addScene = useCallback(async () => {
    // Szenenzahl bewusst zum Aufrufzeitpunkt statt der Render-Closure.
    const scenes = engineStore.getSnapshot().project!.scenes;
    const sceneId = crypto.randomUUID();
    await engineStore.dispatch({
      type: "add_scene",
      sceneId,
      name: `Szene ${scenes.length + 1}`,
    });
    try {
      await engineStore.dispatch({ type: "set_active_scene", sceneId });
    } catch (error) {
      try {
        await engineStore.dispatch({ type: "remove_scene", sceneId });
      } catch (rollback) {
        throw new Error(`${String(error)}; Szenen-Rollback fehlgeschlagen: ${String(rollback)}`);
      }
      throw error;
    }
  }, []);

  const handleAddScene = useCallback(() => void runGuarded(addScene, setSceneError), [addScene]);

  const handleSwitchScene = useCallback((scene: Scene) => {
    void runGuarded(() => engineStore.dispatch({ type: "set_active_scene", sceneId: scene.id }), setSceneError);
  }, []);

  // Aktive Szene bewusst zum Aufrufzeitpunkt, damit der Callback stabil bleibt.
  const handleSaveHotkey = useCallback((event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const value = new FormData(event.currentTarget).get("hotkey");
    const hotkey = typeof value === "string" && value.trim() ? value.trim() : null;
    const snapshot = engineStore.getSnapshot().project!;
    const activeScene = activeSceneOf(snapshot);
    void runGuarded(
      () => engineStore.dispatch({ type: "set_scene_hotkey", sceneId: activeScene.id, hotkey }),
      setHotkeyMessage,
    );
  }, []);

  const addSource = useCallback(async (source: Source, targets: SceneTarget[]) => {
    await engineStore.dispatch({ type: "add_source", source });
    try {
      for (const target of targets) {
        const itemId = crypto.randomUUID();
        await engineStore.dispatch({
          type: "add_scene_item",
          sceneId: target.scene.id,
          itemId,
          sourceId: source.id,
          transform: target.transform,
        });
        if (target.bottom) {
          await engineStore.dispatch({
            type: "reorder_scene_item",
            sceneId: target.scene.id,
            itemId,
            index: 0,
          });
        }
      }
    } catch (error) {
      try {
        await engineStore.dispatch({ type: "remove_source", sourceId: source.id });
      } catch (rollback) {
        throw new Error(`${String(error)}; Quellen-Rollback fehlgeschlagen: ${String(rollback)}`);
      }
      throw error;
    }
    setSelectedSourceId(source.id);
    setAddOpen(false);
  }, []);


  const addCandidate = useCallback(async (candidate: SourceCandidate) => {
    if (candidate.type === "window") {
      await addSource(
        { type: "window", id: crypto.randomUUID(), name: candidate.name, binding: candidate.binding },
        requireSceneTargets("game"),
      );
    } else if (candidate.type === "display") {
      await addSource(
        { type: "display", id: crypto.randomUUID(), name: candidate.name, binding: candidate.binding },
        requireSceneTargets("game"),
      );
    } else {
      const source: Source = {
        type: "application_audio",
        id: crypto.randomUUID(),
        name: candidate.name,
        binding: candidate.binding,
        volume: 1,
        muted: false,
      };
      await addSource(source, []);
    }
  }, []);

  const addTextSource = useCallback(async () => {
    const snapshot = engineStore.getSnapshot().project!;
    const scene = activeSceneOf(snapshot);
    const source: Source = {
      type: "text",
      id: crypto.randomUUID(),
      name: "Text",
      text: "Neuer Text",
      fontFamily: "Inter",
      fontSizePx: 42,
      fontWeight: 600,
      color: "#ffffff",
      backgroundColor: "#000000",
      align: "center",
    };
    await addSource(source, [
      {
        scene,
        transform: {
          ...fullTransform(snapshot.output.width / 2, 160),
          x: snapshot.output.width / 4,
          y: snapshot.output.height / 2 - 80,
        },
      },
    ]);
  }, []);

  const addImageSource = useCallback(async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: "Bild", extensions: ["png", "jpg", "jpeg", "bmp"] }],
    });
    if (typeof path !== "string") return;
    const canonicalPath = await invoke<string>("canonicalize_file", { path });
    const snapshot = engineStore.getSnapshot().project!;
    const scene = activeSceneOf(snapshot);
    const source: Source = {
      type: "image",
      id: crypto.randomUUID(),
      name: canonicalPath.split(/[\\/]/).pop() ?? "Bild",
      path: canonicalPath,
    };
    await addSource(source, [
      {
        scene,
        transform: {
          ...fullTransform(snapshot.output.width / 2, snapshot.output.height / 2),
          x: snapshot.output.width / 4,
          y: snapshot.output.height / 4,
        },
      },
    ]);
  }, []);

  const addMediaSource = useCallback(async () => {
    const path = await open({
      multiple: false,
      filters: [
        { name: "Medien", extensions: ["mp4", "mp3", "wav"] },
      ],
    });
    if (typeof path !== "string") return;
    const canonicalPath = await invoke<string>("canonicalize_file", { path });
    const source: Source = {
      type: "media",
      id: crypto.randomUUID(),
      name: canonicalPath.split(/[\\/]/).pop() ?? "Medium",
      path: canonicalPath,
      loop: false,
      continueWhenHidden: false,
      restartOnShow: false,
      volume: 1,
      muted: false,
    };
    const visual = canonicalPath.toLowerCase().endsWith(".mp4");
    await addSource(source, visual ? requireSceneTargets("video") : []);
  }, []);

  const updateSource = useCallback(async (sourceId: string, changes: Partial<Source>) => {
    const snapshotSource = engineStore
      .getSnapshot()
      .project?.sources.find((source) => source.id === sourceId);
    if (!snapshotSource) {
      pendingSourceFieldsRef.current.delete(sourceId);
      return;
    }
    // Optimistik je Feld: die Basis bleibt stets der autoritative Snapshot; nur
    // noch nicht bestätigte Felder überlagern ihn – auch Lautstärke/Stumm, die
    // über eigene Engine-Befehle laufen (setAudioField).
    const pending: Record<string, unknown> = {
      ...pendingSourceFieldsRef.current.get(sourceId),
      ...changes,
    };
    for (const key of Object.keys(pending)) {
      if (pending[key] === (snapshotSource as unknown as Record<string, unknown>)[key]) delete pending[key];
    }
    if (Object.keys(pending).length === 0) {
      pendingSourceFieldsRef.current.delete(sourceId);
    } else {
      pendingSourceFieldsRef.current.set(sourceId, pending);
    }
    const next = { ...snapshotSource, ...pending } as Source;
    try {
      await sourceMutationQueueRef.current.enqueue(sourceId, () =>
        engineStore.dispatch({ type: "update_source", source: next }),
      );
    } catch (error) {
      if (pendingSourceFieldsRef.current.get(sourceId) === pending) pendingSourceFieldsRef.current.delete(sourceId);
      throw error;
    }
  }, []);

  const seekMedia = useCallback(
    (sourceId: string, positionSeconds: number) =>
      engineStore.dispatch({ type: "media_seek", sourceId, positionSeconds }),
    [],
  );

  const setMediaPlaying = useCallback(
    (sourceId: string, playing: boolean) =>
      engineStore.dispatch({ type: "set_media_playing", sourceId, playing }),
    [],
  );

  const closeDialog = useCallback(() => {
    setAddOpen(false);
    requestAnimationFrame(() => addButton.current?.focus());
  }, []);

  const dismissOnboarding = useCallback(() => {
    onboardingDismissedRef.current = true;
    setOnboarding(false);
  }, []);

  const startOnboarding = useCallback(() => {
    dismissOnboarding();
    setAddOpen(true);
  }, [dismissOnboarding]);

  const openAddDialog = useCallback(() => setAddOpen(true), []);

  // Auswahl wechselt die Quelle und verwirft alte Fehlermeldungen der
  // vorherigen Auswahl, damit sie nicht dem neuen Kontext zugeordnet werden.
  const selectSource = useCallback((sourceId: string) => {
    setSelectedSourceId(sourceId);
    setItemError(null);
    setTextError(null);
  }, []);

  const handleTextChange = useCallback((source: TextSource, event: React.ChangeEvent<HTMLTextAreaElement>) => {
    const textarea = event.currentTarget;
    const attempted = textarea.value;
    setTextError(null);
    void updateSource(source.id, { text: attempted }).catch((error: unknown) => {
      setTextError(String(error));
      // Nur zurücksetzen, wenn keine neuere lokale Eingabe dazwischen liegt.
      if (textarea.value !== attempted) return;
      const authoritative = engineStore.getSnapshot().project?.sources.find((entry) => entry.id === source.id);
      textarea.value = authoritative && authoritative.type === "text" ? authoritative.text : "";
    });
  }, [updateSource]);

  // Item-Aktionen lesen den Snapshot zum Aufrufzeitpunkt; Randklicks der
  // relativen Neuanordnung (±1) sind clientseitig stumme No-Ops, sodass die
  // Beschriftungen „Nach oben“/„Nach unten“ dem Verhalten entsprechen.
  const runItemAction = useCallback((itemId: string, action: ItemAction) => {
    const snapshot = engineStore.getSnapshot().project!;
    const activeScene = activeSceneOf(snapshot);
    const item = activeScene.items.find((entry) => entry.id === itemId);
    if (!item) return;
    let command: EngineCommand;
    if (action === "toggleVisible") {
      command = { type: "set_item_visible", sceneId: activeScene.id, itemId, visible: !item.visible };
    } else if (action === "toggleLocked") {
      command = { type: "set_item_locked", sceneId: activeScene.id, itemId, locked: !item.locked };
    } else {
      const index = activeScene.items.findIndex((entry) => entry.id === itemId);
      const last = activeScene.items.length - 1;
      // Bereits am Zielrand: bewusst kein Befehl und keine Fehlermeldung.
      if ((action === "moveDown" && index <= 0) || (action === "moveUp" && index >= last)) return;
      command = {
        type: "reorder_scene_item",
        sceneId: activeScene.id,
        itemId,
        index: action === "moveUp" ? index + 1 : Math.max(0, index - 1),
      };
    }
    void runGuarded(() => engineStore.dispatch(command), setItemError);
  }, []);

  const removeScene = useCallback(
    (sceneId: string) =>
      runGuarded(() => engineStore.dispatch({ type: "remove_scene", sceneId }), setSceneError),
    [],
  );

  const renameScene = useCallback((sceneId: string, name: string) => {
    void runGuarded(() => engineStore.dispatch({ type: "rename_scene", sceneId, name }), setSceneError);
  }, []);

  // Kaskadierendes Entfernen: die Engine räumt referenzierende Items ab;
  // hier nur die Auswahl und ihre Fehlermeldungen zurücksetzen.
  const removeSource = useCallback((sourceId: string) => {
    return runGuarded(async () => {
      await engineStore.dispatch({ type: "remove_source", sourceId });
      setSelectedSourceId((current) => (current === sourceId ? null : current));
      setTextError(null);
    }, setItemError);
  }, []);

  const quitStudio = useCallback(() => {
    setWindowError(null);
    void getCurrentWindow().close().catch((error: unknown) => {
      setWindowError(`Fensterfehler: ${String(error)}`);
    });
  }, []);

  if (!project) {
    return (
      <main className="loading">
        <h1>Hooviestar</h1>
        <p role="status">{status}</p>
      </main>
    );
  }

  const activeScene = activeSceneOf(project);
  const selectedSource =
    project.sources.find((source) => source.id === selectedSourceId) ?? null;
  const selectedItem = selectedSource
    ? activeScene.items.find((item) => item.sourceId === selectedSource.id) ?? null
    : null;
  const selectedMediaState =
    selectedSource?.type === "media" ? (mediaStates[selectedSource.id] ?? null) : null;

  // Zeilen für das Quellen-Dock: Items der aktiven Szene mit Sichtbarkeits-
  // und Sperrzustand; übrige Quellen erscheinen als „außerhalb der Szene“.
  const sourceRows: SourceRow[] = project.sources.map((source) => {
    const item = activeScene.items.find((entry) => entry.sourceId === source.id);
    return item
      ? { key: item.id, source, itemId: item.id, visible: item.visible, locked: item.locked }
      : { key: source.id, source };
  });

  // Audiokanäle mit Pending-Overlay, damit Slider-Optimistik im Mixer ankommt.
  const mixerChannels = project.sources
    .filter((source): source is Source & { volume: number; muted: boolean } => "volume" in source)
    .map((source) => ({
      sourceId: source.id,
      name: source.name,
      volume: pendingField(source.id, "volume", source.volume),
      muted: pendingField(source.id, "muted", source.muted),
    }));


  /**
   * Rollenzuordnung ausschließlich über die Seed-Hotkeys (umbenennungsfest);
   * fehlt eine Standardszene, wird explizit `null` gemeldet, statt still
   * auf Positionen zurückzufallen.
   */
  function scenesFor(role: "game" | "video"): SceneTarget[] | null {
    // Bewusst den Snapshot zum Aufrufzeitpunkt statt der Render-Closure:
    // so bleiben die Hinzufügen-Callbacks stabil und der Dialog-Memo wirksam.
    const snapshot = engineStore.getSnapshot().project!;
    const scenes = snapshot.scenes;
    const game = scenes.find((scene) => scene.hotkey === GAME_SCENE_HOTKEY);
    const video = scenes.find((scene) => scene.hotkey === VIDEO_SCENE_HOTKEY);
    const both = scenes.find((scene) => scene.hotkey === BOTH_SCENE_HOTKEY);
    if (!game || !video || !both) return null;
    const full = fullTransform(snapshot.output.width, snapshot.output.height);
    if (role === "game") {
      return game.id === both.id
        ? [{ scene: game, transform: full }]
        : [
            { scene: game, transform: full },
            { scene: both, transform: full, bottom: true },
          ];
    }
    return video.id === both.id
      ? [{ scene: video, transform: full }]
      : [
          { scene: video, transform: full },
          { scene: both, transform: pipTransform(snapshot.output) },
        ];
  }

  function requireSceneTargets(role: "game" | "video"): SceneTarget[] {
    const targets = scenesFor(role);
    if (!targets) {
      throw new Error(
        "Standard-Szenen fehlen: „Spiel“ (Ctrl+Alt+1), „Video“ (Ctrl+Alt+2) und „Beides“ (Ctrl+Alt+3) müssen vorhanden sein.",
      );
    }
    return targets;
  }

  return (
    <main className="studio">
      <header className="topbar">
        <div className="brand">
          <h1>Hooviestar</h1>
          <p>Discord-Szenenstudio für Windows und Linux</p>
        </div>
      </header>

      <section className="center-band">
        <PreviewPanel
          output={project.output}
          activeSceneName={activeScene.name}
          onAttachBounds={attachPreviewBounds}
        />
        <div className="preview-strip">
          <span className="strip-label">
            {selectedSource ? selectedSource.name : "Keine Quelle ausgewählt"}
          </span>
          <span className="strip-hint">Discord: App „Hooviestar – Program“ wählen · Ausgabe bleibt unsichtbar</span>
        </div>
      </section>

      <div className="dock-row">
        <ScenesPanel
          scenes={project.scenes}
          activeScene={activeScene}
          sceneError={sceneError}
          hotkeyMessage={hotkeyMessage}
          onAddScene={handleAddScene}
          onSwitchScene={handleSwitchScene}
          onSaveHotkey={handleSaveHotkey}
          onRemoveScene={removeScene}
          onRenameScene={renameScene}
        />
        <SourcesPanel
          rows={sourceRows}
          selectedSourceId={selectedSourceId}
          addButtonRef={addButton}
          onSelectSource={selectSource}
          onAddClick={openAddDialog}
          onRemoveSource={removeSource}
          onItemAction={runItemAction}
        />
        <AudioMixerPanel
          channels={mixerChannels}
          levels={levels}
          audioError={audioError}
          onVolume={setMixerVolume}
          onToggleMute={toggleMixerMute}
        />
        <SourceInspectorPanel
          selectedSource={selectedSource}
          selectedItem={selectedItem}
          mediaState={selectedMediaState}
          itemError={itemError}
          textError={textError}
          onItemAction={runItemAction}
          onTextChange={handleTextChange}
          onAudioField={setAudioField}
          getPendingField={pendingField}
          onUpdateSource={updateSource}
          onSeek={seekMedia}
          onSetPlaying={setMediaPlaying}
        />
        <ControlsDock
          onAddSource={openAddDialog}
          onStartOnboarding={startOnboarding}
          onQuit={quitStudio}
        />
      </div>

      <StatusBar
        status={windowError ?? previewError ?? updateStatus ?? status}
        output={project.output}
        sceneCount={project.scenes.length}
        sourceCount={project.sources.length}
      />

      {onboarding && project.sources.length === 0 && (
        <OnboardingBanner onStart={startOnboarding} onDismiss={dismissOnboarding} />
      )}

      {addOpen && (
        <AddSourceDialog
          onAddText={addTextSource}
          onAddImage={addImageSource}
          onAddMedia={addMediaSource}
          onAddCandidate={addCandidate}
          onEnumerate={engineStore.enumerateSources}
          onSelectPortal={engineStore.selectPortalSources}
          onClose={closeDialog}
        />
      )}
    </main>
  );
}
