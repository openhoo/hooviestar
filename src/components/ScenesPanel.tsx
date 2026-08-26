import { memo, useEffect, useRef, useState } from "react";
import type { Scene } from "../types";
import { MinusIcon, PlusIcon } from "./icons";

interface ScenesPanelProps {
  scenes: Scene[];
  activeScene: Scene;
  sceneError: string | null;
  hotkeyMessage: string | null;
  onAddScene: () => void;
  onSwitchScene: (scene: Scene) => void;
  onSaveHotkey: (event: React.FormEvent<HTMLFormElement>) => void;
  onRemoveScene: (sceneId: string) => void;
  onRenameScene: (sceneId: string, name: string) => void;
}

function ScenesPanelImpl({
  scenes,
  activeScene,
  sceneError,
  hotkeyMessage,
  onAddScene,
  onSwitchScene,
  onSaveHotkey,
  onRemoveScene,
  onRenameScene,
}: ScenesPanelProps) {
  const [removeArmed, setRemoveArmed] = useState(false);
  const removeButtonRef = useRef<HTMLButtonElement>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  // Unterscheidet „Escape“ von „Commit über Blur“, da beim Aushängen des
  // Eingabefelds je nach Browser noch ein Blur-Event nachläuft.
  const renameCancelledRef = useRef(false);

  // Armierung verfällt bei Szenewechsel und bei Klick außerhalb des Buttons.
  useEffect(() => setRemoveArmed(false), [activeScene.id]);
  useEffect(() => {
    if (!removeArmed) return;
    const handle = (event: PointerEvent) => {
      if (event.target instanceof Node && removeButtonRef.current?.contains(event.target)) return;
      setRemoveArmed(false);
    };
    window.addEventListener("pointerdown", handle);
    return () => window.removeEventListener("pointerdown", handle);
  }, [removeArmed]);

  const startRename = (scene: Scene) => {
    renameCancelledRef.current = false;
    setRenamingId(scene.id);
    setRenameDraft(scene.name);
  };

  const commitRename = () => {
    if (renamingId == null) return;
    if (renameCancelledRef.current) {
      renameCancelledRef.current = false;
      setRenamingId(null);
      return;
    }
    const name = renameDraft.trim();
    if (name) onRenameScene(renamingId, name);
    setRenamingId(null);
  };

  return (
    <nav className="dock scenes-dock" aria-label="Szenen">
      <div className="dock-title">
        <h2>Szenen</h2>
        <div className="dock-toolbar">
          <button type="button" className="icon-button" aria-label="Szene hinzufügen" onClick={onAddScene}>
            <PlusIcon />
          </button>
          <button
            ref={removeButtonRef}
            type="button"
            className={removeArmed ? "icon-button armed" : "icon-button"}
            aria-label={removeArmed ? "Erneut klicken zum Entfernen" : "Aktive Szene entfernen"}
            title={removeArmed ? "Erneut klicken zum Entfernen" : "Aktive Szene entfernen"}
            disabled={scenes.length <= 1}
            onClick={() => {
              if (removeArmed) {
                setRemoveArmed(false);
                onRemoveScene(activeScene.id);
              } else {
                setRemoveArmed(true);
              }
            }}
          >
            <MinusIcon />
          </button>
        </div>
      </div>
      <ol className="scenes-list">
        {scenes.map((scene) => (
          <li key={scene.id}>
            {renamingId === scene.id ? (
              <input
                className="rename-input"
                autoFocus
                value={renameDraft}
                onChange={(event) => setRenameDraft(event.currentTarget.value)}
                onBlur={commitRename}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    commitRename();
                  } else if (event.key === "Escape") {
                    renameCancelledRef.current = true;
                    setRenamingId(null);
                  }
                }}
              />
            ) : (
              <button
                type="button"
                className={scene.id === activeScene.id ? "scene-row selected" : "scene-row"}
                onClick={() => onSwitchScene(scene)}
              >
                <span
                  className="scene-name"
                  title={`${scene.name} (Doppelklick zum Umbenennen)`}
                  onDoubleClick={() => startRename(scene)}
                >
                  {scene.name}
                </span>
                <kbd>{scene.hotkey ?? "–"}</kbd>
              </button>
            )}
          </li>
        ))}
      </ol>
      <form className="hotkey-editor" onSubmit={onSaveHotkey}>
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
      {sceneError && <p role="alert">{sceneError}</p>}
    </nav>
  );
}

export const ScenesPanel = memo(ScenesPanelImpl);
