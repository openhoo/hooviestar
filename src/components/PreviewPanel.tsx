import { memo } from "react";
import type { OutputConfig } from "../types";
import { isWindowsPlatform } from "../platform";

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
            <span>{isWindowsPlatform() ? "Native D3D11-Vorschau" : "Vulkan-Ausgabe läuft im Hintergrund"}</span>
          </div>
        </div>
      </section>
    </>
  );
}

export const PreviewPanel = memo(PreviewPanelImpl);
