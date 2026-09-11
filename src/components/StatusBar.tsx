import { memo } from "react";
import type { OutputConfig } from "../types";
import { statusTone } from "../engineStore";
import { updateStatusMessage, updateStatusTone } from "../updateStatus";
import type { UpdateStatusEvent } from "../updateStatus";

interface StatusBarProps {
  status?: string | null;
  updateStatus: UpdateStatusEvent | null;
  onInstallUpdate: () => void;
  installUpdateBusy: boolean;
  installUpdateError?: string | null;
  output?: OutputConfig;
  sceneCount?: number;
  sourceCount?: number;
}

interface UpdateStatusItemProps {
  updateStatus: UpdateStatusEvent;
  onInstallUpdate: () => void;
  installUpdateBusy: boolean;
  installUpdateError?: string | null;
}

function UpdateStatusItem({
  updateStatus,
  onInstallUpdate,
  installUpdateBusy,
  installUpdateError,
}: UpdateStatusItemProps) {
  const message = updateStatusMessage(updateStatus);
  const tone = updateStatusTone(updateStatus);
  const hasError =
    tone === "error" || (installUpdateError !== undefined && installUpdateError !== null);
  const showInstallError =
    installUpdateError !== undefined &&
    installUpdateError !== null &&
    (updateStatus.status !== "error" || updateStatus.message !== installUpdateError);
  const downloadingStatus = updateStatus.status === "downloading" ? updateStatus : null;
  const downloading = downloadingStatus !== null;
  const ready = updateStatus.status === "ready";
  const automaticTransition =
    ready || updateStatus.status === "installing" || updateStatus.status === "installed";

  return (
    <div
      className={`status-item status-update ${hasError ? "error" : ""}`}
      role="group"
      aria-label="Aktualisierungsstatus"
      aria-busy={installUpdateBusy}
    >
      <span className={hasError ? "status-dot error" : "status-dot"} aria-hidden="true" />
      <span className="status-update-copy">
        <span
          className={`status-message status-update-message${automaticTransition ? " status-update-warning" : ""}`}
          role={tone === "error" ? "alert" : "status"}
          aria-live={downloading ? "off" : tone === "error" ? "assertive" : "polite"}
          title={message}
        >
          {message}
        </span>
        {showInstallError && (
          <span className="status-update-local-error" role="alert">
            {" · "}Installation fehlgeschlagen: {installUpdateError}
          </span>
        )}
      </span>
      {downloadingStatus && downloadingStatus.progress === null && (
        <progress
          className="status-update-progress"
          max={100}
          aria-label={`Fortschritt der Aktualisierung ${downloadingStatus.version} (unbekannt)`}
          aria-valuetext="Fortschritt unbekannt"
        >
          Fortschritt unbekannt
        </progress>
      )}
      {downloadingStatus && downloadingStatus.progress !== null && (
        <progress
          className="status-update-progress"
          max={100}
          value={downloadingStatus.progress}
          aria-label={`Fortschritt der Aktualisierung ${downloadingStatus.version}: ${downloadingStatus.progress} %`}
          aria-valuetext={`${downloadingStatus.progress} %`}
        >
          {downloadingStatus.progress} %
        </progress>
      )}
      {ready && (
        <button
          type="button"
          className="status-update-install"
          onClick={onInstallUpdate}
          disabled={installUpdateBusy}
        >
          {installUpdateBusy ? "Installation wird vorbereitet …" : "Installieren und neu starten"}
        </button>
      )}
    </div>
  );
}

function StatusBarImpl({
  status,
  updateStatus,
  onInstallUpdate,
  installUpdateBusy,
  installUpdateError,
  output,
  sceneCount,
  sourceCount,
}: StatusBarProps) {
  const hasProjectSummary = output !== undefined && sceneCount !== undefined && sourceCount !== undefined;
  const compact = !hasProjectSummary;
  const className = [
    "status-bar",
    updateStatus ? "has-update" : "",
    compact ? "status-bar-compact" : "",
  ].filter(Boolean).join(" ");

  return (
    <footer className={className}>
      {status && (
        <span className="status-item status-engine">
          <span className={statusTone(status) === "error" ? "status-dot error" : "status-dot"} aria-hidden="true" />
          <span className="status-message" role="status" aria-live="polite" title={status}>{status}</span>
        </span>
      )}
      {updateStatus && (
        <UpdateStatusItem
          updateStatus={updateStatus}
          onInstallUpdate={onInstallUpdate}
          installUpdateBusy={installUpdateBusy}
          installUpdateError={installUpdateError}
        />
      )}
      {hasProjectSummary && (
        <span className="status-item status-counts">
          {sceneCount} {sceneCount === 1 ? "Szene" : "Szenen"} · {sourceCount} {sourceCount === 1 ? "Quelle" : "Quellen"}
        </span>
      )}
      {output && (
        <span className="status-item status-output">
          {output.width}×{output.height} · {output.fps} fps
        </span>
      )}
    </footer>
  );
}

export const StatusBar = memo(StatusBarImpl);
