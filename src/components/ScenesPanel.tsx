import { memo } from "react";
import type { Scene } from "../types";

interface ScenesPanelProps {
  scenes: Scene[];
  activeScene: Scene;
  sceneError: string | null;
  hotkeyMessage: string | null;
  onAddScene: () => void;
  onSwitchScene: (scene: Scene) => void;
  onSaveHotkey: (event: React.FormEvent<HTMLFormElement>) => void;
}

function ScenesPanelImpl({
  scenes,
  activeScene,
  sceneError,
  hotkeyMessage,
  onAddScene,
  onSwitchScene,
  onSaveHotkey,
}: ScenesPanelProps) {
  return (
    <nav className="panel scenes" aria-label="Szenen">
      <div className="panel-title">
        <h2>Szenen</h2>
        <button aria-label="Szene hinzufügen" onClick={onAddScene}>+</button>
      </div>
      <ol>
        {scenes.map((scene) => (
          <li key={scene.id}>
            <button
              className={scene.id === activeScene.id ? "selected" : ""}
              onClick={() => onSwitchScene(scene)}
            >
              <span>{scene.name}</span><kbd>{scene.hotkey ?? "–"}</kbd>
            </button>
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
