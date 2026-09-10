import { memo, useEffect, useRef, useState } from "react";
import type { Scene } from "../types";
import { ConfirmDialog } from "./ConfirmDialog";
import { MinusIcon, PencilIcon, PlusIcon } from "./icons";

function allowsDeleteShortcut(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (
    target.isContentEditable ||
    target.tagName === "INPUT" ||
    target.tagName === "TEXTAREA" ||
    target.tagName === "SELECT"
  ) {
    return false;
  }
  return !target.closest('[role="dialog"], [aria-modal="true"], .modal-dialog');
}

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
  onRemoveArmedChange?: (armed: boolean) => void;
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
  onRemoveArmedChange,
}: ScenesPanelProps) {
  const removeButtonRef = useRef<HTMLButtonElement>(null);
  const [removeOpen, setRemoveOpen] = useState(false);
  const [removeSceneId, setRemoveSceneId] = useState<string | null>(null);
  const [removeBusy, setRemoveBusy] = useState(false);
  const removeBusyRef = useRef(false);
  const [removeError, setRemoveError] = useState<string | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  // Unterscheidet „Escape“ von „Commit über Blur“, da beim Aushängen des
  // Eingabefelds je nach Browser noch ein Blur-Event nachläuft.
  const renameCancelledRef = useRef(false);

  useEffect(() => {
    onRemoveArmedChange?.(removeOpen);
  }, [onRemoveArmedChange, removeOpen]);
  useEffect(() => () => onRemoveArmedChange?.(false), [onRemoveArmedChange]);

  // Ein externer Szenenwechsel darf eine bereits geöffnete Bestätigung nicht
  // auf eine inzwischen andere aktive Szene anwenden.
  useEffect(() => {
    if (removeOpen && removeSceneId !== activeScene.id) {
      setRemoveOpen(false);
      setRemoveSceneId(null);
      setRemoveError(null);
    }
  }, [activeScene.id, removeOpen, removeSceneId]);

  function requestRemove() {
    if (scenes.length <= 1 || removeBusyRef.current) return;
    setRemoveSceneId(activeScene.id);
    setRemoveError(null);
    setRemoveOpen(true);
  }

  function cancelRemove() {
    if (removeBusyRef.current) return;
    setRemoveOpen(false);
    setRemoveSceneId(null);
    setRemoveError(null);
  }

  async function confirmRemove() {
    if (removeBusyRef.current) return;
    const targetId = removeSceneId;
    const target = targetId ? scenes.find((scene) => scene.id === targetId) : undefined;
    if (!target || target.id !== activeScene.id || scenes.length <= 1) {
      setRemoveError("Die Szene ist nicht mehr aktiv. Bitte die Entfernung erneut starten.");
      return;
    }
    removeBusyRef.current = true;
    setRemoveBusy(true);
    try {
      await onRemoveScene(target.id);
      setRemoveOpen(false);
      setRemoveSceneId(null);
    } catch (error) {
      setRemoveError(String(error));
    } finally {
      removeBusyRef.current = false;
      setRemoveBusy(false);
    }
  }
  function handleDockKeyDown(event: React.KeyboardEvent<HTMLElement>) {
    if (
      event.defaultPrevented ||
      (event.key !== "Delete" && event.key !== "Backspace") ||
      !allowsDeleteShortcut(event.target) ||
      scenes.length <= 1
    ) {
      return;
    }
    const target = event.target as HTMLElement;
    const activeRow = target.closest(".scene-row");
    const removeControl = target.closest('[data-delete-trigger="scene"]');
    if (activeRow?.getAttribute("aria-current") !== "true" && !removeControl) return;
    event.preventDefault();
    event.stopPropagation();
    requestRemove();
  }

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

  const removeTargetName = removeSceneId
    ? scenes.find((scene) => scene.id === removeSceneId)?.name ?? activeScene.name
    : activeScene.name;

  return (
    <nav className="dock scenes-dock" aria-label="Szenen" aria-keyshortcuts="Delete" onKeyDown={handleDockKeyDown}>
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
            className={removeOpen ? "icon-button armed" : "icon-button"}
            aria-label="Aktive Szene entfernen"
            aria-keyshortcuts="Delete"
            aria-expanded={removeOpen}
            title="Aktive Szene entfernen"
            data-delete-trigger="scene"
            disabled={scenes.length <= 1}
            onClick={requestRemove}
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
                  onDoubleClick={() => startRename(scene)}
                >
                  <span
                    className="scene-name"
                    title={`${scene.name} (Doppelklick zum Umbenennen)`}
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
      <ConfirmDialog
        open={removeOpen}
        title={`Szene „${removeTargetName}“ entfernen?`}
        description="Die aktive Szene wird entfernt. Die erste verbleibende Szene wird anschließend aktiv."
        error={removeError}
        busy={removeBusy}
        onCancel={cancelRemove}
        onConfirm={confirmRemove}
        onRestoreFocus={() => removeButtonRef.current?.focus()}
      />
    </nav>
  );
}

export const ScenesPanel = memo(ScenesPanelImpl);
