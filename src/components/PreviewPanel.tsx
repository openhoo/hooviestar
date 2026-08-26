import { memo } from "react";
import type { OutputConfig } from "../types";

interface PreviewPanelProps {
  output: OutputConfig;
  activeSceneName: string;
  onAttachBounds: (node: HTMLDivElement | null) => void;
}

function PreviewPanelImpl({ output, activeSceneName, onAttachBounds }: PreviewPanelProps) {
  return (
    <>
      {/* Die Bühne zentriert; der Rahmen trägt das Seitenverhältnis der Ausgabe,
          damit die nativen Preview-Bounds exakt dem Videobereich folgen. */}
      <section className="preview-stage">
        <div
          id="native-preview-bounds"
          ref={onAttachBounds}
          className="preview-frame"
          style={{ aspectRatio: `${output.width} / ${output.height}` }}
          aria-label="Native Szenenvorschau"
        >
          <div className="preview-placeholder">
            <strong>{activeSceneName}</strong>
            <span>{navigator.platform.toLowerCase().includes("win") ? "Native D3D11-Vorschau" : "Separates Vulkan-Preview-Fenster"}</span>
          </div>
        </div>
      </section>
    </>
  );
}

export const PreviewPanel = memo(PreviewPanelImpl);
