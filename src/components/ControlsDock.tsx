import { memo } from "react";
import { PowerIcon, SourcePlusIcon, SparkleIcon } from "./icons";

interface ControlsDockProps {
  onAddSource: () => void;
  onStartOnboarding: () => void;
  onQuit: () => void;
}

function ControlsDockImpl({ onAddSource, onStartOnboarding, onQuit }: ControlsDockProps) {
  return (
    <section className="dock controls-dock" aria-label="Steuerpult">
      <div className="dock-title">
        <h2>Steuerpult</h2>
      </div>
      <button type="button" className="control-button" onClick={onAddSource}>
        <SourcePlusIcon />
        <span>Quelle hinzufügen</span>
      </button>
      <button type="button" className="control-button" onClick={onStartOnboarding}>
        <SparkleIcon />
        <span>Einrichtung starten</span>
      </button>
      <button type="button" className="control-button" onClick={onQuit}>
        <PowerIcon />
        <span>Studio beenden</span>
      </button>
    </section>
  );
}

export const ControlsDock = memo(ControlsDockImpl);
