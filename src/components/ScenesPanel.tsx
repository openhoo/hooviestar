import { memo, useMemo, useRef, useState } from "react";
import type { Scene } from "../types";
import { MinusIcon, PencilIcon, PlusIcon } from "./icons";
import { useArmedConfirm } from "../hooks/useArmedConfirm";

interface ScenesPanelProps {
  scenes: Scene[];
  activeScene: Scene;
  sceneError: string | null;
  hotkeyMessage: string | null;
  onAddScene: () => void;
  onSwitchScene: (scene: Scene) => void;
  onSaveHotkey: (event: React.FormEvent<HTMLFormElement>) => void;
  onRemoveScene: (sceneId: string) => void | Promise<void>;
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
  const removeButtonRef = useRef<HTMLButtonElement>(null);
  const removeTriggerRefs = useMemo(() => [removeButtonRef], []);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  // Unterscheidet „Escape“ von „Commit über Blur“, da beim Aushängen des
  // Eingabefelds je nach Browser noch ein Blur-Event nachläuft.
  const renameCancelledRef = useRef(false);

  // Geteilte Zwei-Klick-Entfernung: Szenewechsel, Klick außerhalb und Escape
  // entschärfen; während der Bestätigung werden weitere Klicks ignoriert.
  const { armed: removeArmed, trigger: triggerRemove } = useArmedConfirm(
    () => onRemoveScene(activeScene.id),
    activeScene.id,
    removeTriggerRefs,
  );

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
        <div className="dock-heading">
          <h2>Szenen</h2>
          <span>{scenes.length}</span>
        </div>
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
            onClick={triggerRemove}
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
                aria-label={`Szene „${scene.name}“ umbenennen`}
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
              <div className={scene.id === activeScene.id ? "scene-item selected" : "scene-item"}>
                <button
                  type="button"
                  className="scene-row"
                  aria-current={scene.id === activeScene.id ? "true" : undefined}
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
                <button
                  type="button"
                  className="icon-button rename-button"
                  aria-label={`Szene „${scene.name}“ umbenennen`}
                  title="Szene umbenennen"
                  onClick={() => startRename(scene)}
                >
                  <PencilIcon />
                </button>
              </div>
            )}
          </li>
        ))}
      </ol>
      <details className="hotkey-settings" key={activeScene.id}>
        <summary>
          <span>Hotkey bearbeiten</span>
          <kbd>{activeScene.hotkey ?? "Nicht gesetzt"}</kbd>
        </summary>
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
      </details>
      {sceneError && <p className="dock-message" role="alert">{sceneError}</p>}
    </nav>
  );
}

export const ScenesPanel = memo(ScenesPanelImpl);
