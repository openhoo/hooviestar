import { memo } from "react";
import type { OutputConfig } from "../types";
import { statusTone } from "../engineStore";

interface StatusBarProps {
  status: string;
  output: OutputConfig;
  sceneCount: number;
  sourceCount: number;
}

function StatusBarImpl({ status, output, sceneCount, sourceCount }: StatusBarProps) {
  return (
    <footer className="status-bar">
      <span className="status-item">
        <span className={statusTone(status) === "error" ? "status-dot error" : "status-dot"} aria-hidden="true" />
        <span className="status-message" role="status" aria-live="polite" title={status}>{status}</span>
      </span>
      <span className="status-item status-counts">
        {sceneCount} {sceneCount === 1 ? "Szene" : "Szenen"} · {sourceCount} {sourceCount === 1 ? "Quelle" : "Quellen"}
      </span>
      <span className="status-item status-output">
        {output.width}×{output.height} · {output.fps} fps
      </span>
    </footer>
  );
}

export const StatusBar = memo(StatusBarImpl);
