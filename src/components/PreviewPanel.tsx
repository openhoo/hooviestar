import { memo } from "react";
import type { OutputConfig } from "../types";

interface PreviewPanelProps {
  output: OutputConfig;
  activeSceneName: string;
  onAttachBounds: (node: HTMLDivElement | null) => void;
}

function PreviewPanelImpl({ output, activeSceneName, onAttachBounds }: PreviewPanelProps) {
  return (
    <section className="center">
      <div className="panel preview-panel">
        <div className="panel-title">
          <h2>Vorschau</h2>
          <span>{output.width}×{output.height} · {output.fps} fps</span>
        </div>
        <div id="native-preview-bounds" ref={onAttachBounds} className="preview" aria-label="Native Szenenvorschau">
          <div className="preview-placeholder">
            <strong>{activeSceneName}</strong>
            <span>{navigator.platform.toLowerCase().includes("win") ? "Native D3D11-Vorschau" : "Separates Vulkan-Preview-Fenster"}</span>
          </div>
        </div>
      </div>
      <div className="share-callout">
        <strong>In Discord teilen:</strong>
        <span>Fenster „Hooviestar – Program“ auswählen. Nicht das Studio teilen.</span>
      </div>
    </section>
  );
}

export const PreviewPanel = memo(PreviewPanelImpl);
