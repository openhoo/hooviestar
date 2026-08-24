import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { engineStore } from "./engineStore";
import { pipTransform } from "./types";
import type { Scene, Source, SourceCandidate, Transform } from "./types";

function sourceLabel(source: Source) {
  if (source.type === "application_audio") return "Anwendungs-Audio";
  if (source.type === "window") return "Fenster";
  if (source.type === "display") return "Monitor";
  if (source.type === "image") return "Bild";
  if (source.type === "text") return "Text";
  return "Medium";
}

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

export default function App() {
  const { project, status, levels, mediaStates } = useSyncExternalStore(
    engineStore.subscribe,
    engineStore.getSnapshot,
  );
  const [selectedSourceId, setSelectedSourceId] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const [onboarding, setOnboarding] = useState(false);
  const [candidates, setCandidates] = useState<SourceCandidate[]>([]);
  const [sourceMessage, setSourceMessage] = useState<string | null>(null);
  const [portalRequired, setPortalRequired] = useState(false);
  const [sourceLoading, setSourceLoading] = useState(false);
  const [hotkeyMessage, setHotkeyMessage] = useState<string | null>(null);
  const addButton = useRef<HTMLButtonElement>(null);
  const dialog = useRef<HTMLElement>(null);

  useEffect(() => {
    void engineStore.start();
  }, []);

  useEffect(() => {
    if (!project) return;
    if (project.sources.length === 0) setOnboarding(true);
  }, [project]);

  useEffect(() => {
    if (!project || !navigator.platform.toLowerCase().includes("win")) return;
    const preview = document.getElementById("native-preview-bounds");
    if (!preview) return;
    const report = () => {
      const bounds = preview.getBoundingClientRect();
      void invoke("set_preview_bounds", {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
      });
    };
    const observer = new ResizeObserver(report);
    observer.observe(preview);
    report();
    return () => observer.disconnect();
  }, [project]);

  useEffect(() => {
    if (!addOpen) return;
    setSourceLoading(true);
    void engineStore
      .enumerateSources()
      .then((result) => {
        setCandidates(result.candidates);
        setPortalRequired(result.portalSelectionRequired);
        setSourceMessage(result.message);
      })
      .catch((error) => setSourceMessage(String(error)))
      .finally(() => setSourceLoading(false));
    requestAnimationFrame(() => {
      dialog.current?.querySelector<HTMLElement>("button")?.focus();
    });
  }, [addOpen]);

  if (!project) {
    return (
      <main className="loading">
        <h1>Hooviestar</h1>
        <p role="status">{status}</p>
      </main>
    );
  }

  const activeScene =
    project.scenes.find((scene) => scene.id === project.activeSceneId) ?? project.scenes[0];
  const selectedSource =
    project.sources.find((source) => source.id === selectedSourceId) ?? null;
  const selectedItem = selectedSource
    ? activeScene.items.find((item) => item.sourceId === selectedSource.id) ?? null
    : null;
  const selectedMediaState =
    selectedSource?.type === "media" ? (mediaStates[selectedSource.id] ?? null) : null;

  async function addScene() {
    const sceneId = crypto.randomUUID();
    await engineStore.dispatch({
      type: "add_scene",
      sceneId,
      name: `Szene ${project!.scenes.length + 1}`,
    });
    await engineStore.dispatch({ type: "set_active_scene", sceneId });
  }

  async function switchScene(scene: Scene) {
    await engineStore.dispatch({ type: "set_active_scene", sceneId: scene.id });
  }

  async function saveSceneHotkey(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = new FormData(event.currentTarget).get("hotkey");
    const hotkey = typeof value === "string" && value.trim() ? value.trim() : null;
    try {
      await engineStore.dispatch({
        type: "set_scene_hotkey",
        sceneId: activeScene.id,
        hotkey,
      });
      setHotkeyMessage(null);
    } catch (error) {
      setHotkeyMessage(String(error));
    }
  }

  async function addSource(source: Source, targets: Array<{ scene: Scene; transform: Transform; bottom?: boolean }>) {
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
      await engineStore.dispatch({ type: "remove_source", sourceId: source.id });
      throw error;
    }
    setSelectedSourceId(source.id);
    setAddOpen(false);
  }

  function scenesFor(role: "game" | "video") {
    const game = project!.scenes.find((scene) => scene.name === "Spiel") ?? project!.scenes[0];
    const video =
      project!.scenes.find((scene) => scene.name === "Video") ?? project!.scenes[1] ?? game;
    const both =
      project!.scenes.find((scene) => scene.name === "Beides") ?? project!.scenes[2] ?? game;
    const full = fullTransform(project!.output.width, project!.output.height);
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
          { scene: both, transform: pipTransform(project!.output) },
        ];
  }

  async function addCandidate(candidate: SourceCandidate) {
    if (candidate.type === "window") {
      await addSource(
        { type: "window", id: crypto.randomUUID(), name: candidate.name, binding: candidate.binding },
        scenesFor("game"),
      );
    } else if (candidate.type === "display") {
      await addSource(
        { type: "display", id: crypto.randomUUID(), name: candidate.name, binding: candidate.binding },
        scenesFor("game"),
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
  }

  async function addTextSource() {
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
        scene: activeScene,
        transform: {
          ...fullTransform(project!.output.width / 2, 160),
          x: project!.output.width / 4,
          y: project!.output.height / 2 - 80,
        },
      },
    ]);
  }

  async function addImageSource() {
    const path = await open({
      multiple: false,
      filters: [{ name: "Bild", extensions: ["png", "jpg", "jpeg", "bmp"] }],
    });
    if (typeof path !== "string") return;
    const canonicalPath = await invoke<string>("canonicalize_file", { path });
    const source: Source = {
      type: "image",
      id: crypto.randomUUID(),
      name: canonicalPath.split(/[\\/]/).pop() ?? "Bild",
      path: canonicalPath,
    };
    await addSource(source, [
      {
        scene: activeScene,
        transform: {
          ...fullTransform(project!.output.width / 2, project!.output.height / 2),
          x: project!.output.width / 4,
          y: project!.output.height / 4,
        },
      },
    ]);
  }

  async function addMediaSource() {
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
    await addSource(source, visual ? scenesFor("video") : []);
  }

  async function updateSource(sourceId: string, changes: Partial<Source>) {
    const latestSource = engineStore
      .getSnapshot()
      .project?.sources.find((source) => source.id === sourceId);
    if (!latestSource) return;
    await engineStore.dispatch({
      type: "update_source",
      source: { ...latestSource, ...changes } as Source,
    });
  }

  async function selectPortalSources() {
    setSourceLoading(true);
    try {
      const result = await engineStore.selectPortalSources();
      setCandidates(result.candidates);
      setSourceMessage(result.message);
      setPortalRequired(result.portalSelectionRequired);
    } catch (error) {
      setSourceMessage(String(error));
    } finally {
      setSourceLoading(false);
    }
  }

  function closeDialog() {
    setAddOpen(false);
    requestAnimationFrame(() => addButton.current?.focus());
  }

  function trapDialogKeys(event: React.KeyboardEvent<HTMLElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeDialog();
      return;
    }
    if (event.key !== "Tab") return;
    const controls = Array.from(
      event.currentTarget.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)"),
    );
    if (controls.length === 0) return;
    const first = controls[0];
    const last = controls[controls.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  return (
    <main className="studio">
      <header className="topbar">
        <div>
          <h1>Hooviestar</h1>
          <p>Discord-Szenenstudio für Windows und Linux</p>
        </div>
        <div className="program-state">
          <span className="status-dot" /> <span role="status">{status}</span>
        </div>
      </header>

      <nav className="panel scenes" aria-label="Szenen">
        <div className="panel-title">
          <h2>Szenen</h2>
          <button aria-label="Szene hinzufügen" onClick={() => void addScene()}>+</button>
        </div>
        <ol>
          {project.scenes.map((scene) => (
            <li key={scene.id}>
              <button
                className={scene.id === activeScene.id ? "selected" : ""}
                onClick={() => void switchScene(scene)}
              >
                <span>{scene.name}</span><kbd>{scene.hotkey ?? "–"}</kbd>
              </button>
            </li>
          ))}
        </ol>
        <form className="hotkey-editor" onSubmit={(event) => void saveSceneHotkey(event)}>
          <label htmlFor="scene-hotkey">Hotkey für {activeScene.name}</label>
          <div>
            <input
              id="scene-hotkey"
              name="hotkey"
              key={`${activeScene.id}:${activeScene.hotkey ?? ""}`}
              defaultValue={activeScene.hotkey ?? ""}
              placeholder="Ctrl+Alt+1"
              autoComplete="off"
            />
            <button type="submit">Setzen</button>
          </div>
          {hotkeyMessage && <p role="alert">{hotkeyMessage}</p>}
        </form>
      </nav>

      <section className="center">
        <div className="panel preview-panel">
          <div className="panel-title">
            <h2>Vorschau</h2>
            <span>{project.output.width}×{project.output.height} · {project.output.fps} fps</span>
          </div>
          <div id="native-preview-bounds" className="preview" tabIndex={0} aria-label="Native Szenenvorschau">
            <div className="preview-placeholder">
              <strong>{activeScene.name}</strong>
              <span>{navigator.platform.toLowerCase().includes("win") ? "Native D3D11-Vorschau" : "Separates Vulkan-Preview-Fenster"}</span>
            </div>
          </div>
          <p className="hint">Auswahl ziehen · Alt + Rand beschneidet · Strg deaktiviert Einrasten</p>
        </div>
        <div className="share-callout">
          <strong>In Discord teilen:</strong>
          <span>Fenster „Hooviestar – Program“ auswählen. Nicht das Studio teilen.</span>
        </div>
      </section>

      <aside className="panel inspector">
        <div className="panel-title">
          <h2>Quellen</h2>
          <button ref={addButton} onClick={() => setAddOpen(true)}>Hinzufügen</button>
        </div>
        <ul className="source-list">
          {project.sources.map((source) => (
            <li key={source.id}>
              <button
                className={source.id === selectedSourceId ? "selected" : ""}
                onClick={() => setSelectedSourceId(source.id)}
              >
                <span className="source-type">{sourceLabel(source)}</span>
                <strong>{source.name}</strong>
              </button>
            </li>
          ))}
        </ul>
        {selectedSource ? (
          <div className="properties">
            <h3>Eigenschaften</h3>
            <label>Name<input value={selectedSource.name} readOnly /></label>
            {selectedItem && (
              <div className="property-actions">
                <button onClick={() => void engineStore.dispatch({ type: "set_item_visible", sceneId: activeScene.id, itemId: selectedItem.id, visible: !selectedItem.visible })}>{selectedItem.visible ? "Ausblenden" : "Einblenden"}</button>
                <button onClick={() => void engineStore.dispatch({ type: "set_item_locked", sceneId: activeScene.id, itemId: selectedItem.id, locked: !selectedItem.locked })}>{selectedItem.locked ? "Entsperren" : "Sperren"}</button>
                <button onClick={() => void engineStore.dispatch({ type: "reorder_scene_item", sceneId: activeScene.id, itemId: selectedItem.id, index: activeScene.items.length - 1 })}>Nach oben</button>
                <button onClick={() => void engineStore.dispatch({ type: "reorder_scene_item", sceneId: activeScene.id, itemId: selectedItem.id, index: 0 })}>Nach unten</button>
              </div>
            )}
            {selectedSource.type === "text" && (
              <label>Text<textarea value={selectedSource.text} onChange={(event) => void updateSource(selectedSource.id, { text: event.currentTarget.value })} /></label>
            )}
            {"volume" in selectedSource && (
              <>
                <label>Lautstärke <output>{Math.round(selectedSource.volume * 100)} %</output><input type="range" min="0" max="1" step="0.01" value={selectedSource.volume} onChange={(event) => void engineStore.dispatch({ type: "set_audio_volume", sourceId: selectedSource.id, volume: Number(event.currentTarget.value) })} /></label>
                <label className="check"><input type="checkbox" checked={selectedSource.muted} onChange={(event) => void engineStore.dispatch({ type: "set_audio_muted", sourceId: selectedSource.id, muted: event.currentTarget.checked })} /> Stumm</label>
              </>
            )}
            {selectedSource.type === "media" && (
              <div className="media-controls">
                <button onClick={() => void engineStore.dispatch({ type: "set_media_playing", sourceId: selectedSource.id, playing: !(selectedMediaState?.playing ?? true) })}>{selectedMediaState?.playing === false ? "Wiedergabe" : "Pause"}</button>
                <label>Position (Sekunden)<input type="number" min="0" step="1" value={Math.round(selectedMediaState?.positionSeconds ?? 0)} onChange={(event) => void engineStore.dispatch({ type: "media_seek", sourceId: selectedSource.id, positionSeconds: Number(event.currentTarget.value) })} /></label>
                <label className="check"><input type="checkbox" checked={selectedSource.loop} onChange={(event) => void updateSource(selectedSource.id, { loop: event.currentTarget.checked })} /> Wiederholen</label>
                <label className="check"><input type="checkbox" checked={selectedSource.continueWhenHidden} onChange={(event) => void updateSource(selectedSource.id, { continueWhenHidden: event.currentTarget.checked })} /> Versteckt weiterlaufen</label>
                <label className="check"><input type="checkbox" checked={selectedSource.restartOnShow} onChange={(event) => void updateSource(selectedSource.id, { restartOnShow: event.currentTarget.checked })} /> Beim Einblenden neu starten</label>
              </div>
            )}
          </div>
        ) : <p className="empty">Quelle auswählen, um Eigenschaften zu bearbeiten.</p>}
      </aside>

      <section className="panel mixer" aria-label="Audiomixer">
        <div className="panel-title"><h2>Audiomixer</h2><span>48 kHz · Stereo</span></div>
        <div className="mixer-grid">
          {project.sources.filter((source) => "volume" in source).map((source) => (
            <div className="channel" key={source.id}>
              <strong>{source.name}</strong>
              <div className="meter" aria-label={`Pegel ${source.name}`}><i style={{ width: `${Math.min(100, Math.round((levels.find((entry) => entry.sourceId === source.id)?.peak ?? 0) * 100))}%` }} /></div>
              <button onClick={() => void engineStore.dispatch({ type: "set_audio_muted", sourceId: source.id, muted: !source.muted })}>{source.muted ? "Ton an" : "Stumm"}</button>
            </div>
          ))}
          {!project.sources.some((source) => "volume" in source) && <p className="empty">Noch keine Audioquelle.</p>}
        </div>
      </section>

      {onboarding && project.sources.length === 0 && (
        <div className="onboarding-banner" role="region" aria-label="Ersteinrichtung">
          <div><strong>Spiel, Video und Beides sind vorbereitet.</strong><span>Füge zuerst ein Fenster oder einen Monitor und danach ein Medium hinzu.</span></div>
          <button onClick={() => { setOnboarding(false); setAddOpen(true); }}>Einrichtung starten</button>
          <button onClick={() => setOnboarding(false)}>Später</button>
        </div>
      )}

      {addOpen && (
        <div className="dialog-backdrop" role="presentation">
          <section ref={dialog} className="dialog" role="dialog" aria-modal="true" aria-labelledby="add-title" onKeyDown={trapDialogKeys}>
            <div className="panel-title"><h2 id="add-title">Quelle hinzufügen</h2><button aria-label="Schließen" onClick={closeDialog}>×</button></div>
            <div className="source-options">
              <button onClick={() => void addTextSource()}><strong>Text</strong><span>GPU-gerenderte Beschriftung</span></button>
              <button onClick={() => void addImageSource()}><strong>Bild</strong><span>PNG, JPEG oder BMP</span></button>
              <button onClick={() => void addMediaSource()}><strong>Medium</strong><span>MP4, MP3 oder WAV</span></button>
              {sourceLoading && <p role="status">Quellen werden gesucht…</p>}
              {portalRequired && <button onClick={() => void selectPortalSources()}><strong>Fenster oder Monitor auswählen</strong><span>Desktop-Portal öffnen</span></button>}
              {candidates.map((candidate) => (
                <button key={`${candidate.type}:${candidate.runtimeId}`} onClick={() => void addCandidate(candidate)}>
                  <strong>{candidate.name}</strong>
                  <span>{candidate.type === "window" ? "Fenster" : candidate.type === "display" ? "Monitor" : "Anwendungs-Audio"}</span>
                </button>
              ))}
              {sourceMessage && <p className="source-message">{sourceMessage}</p>}
            </div>
          </section>
        </div>
      )}
    </main>
  );
}
