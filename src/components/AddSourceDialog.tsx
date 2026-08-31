import { memo, useEffect, useRef, useState } from "react";
import type { SourceCandidate, SourceEnumeration } from "../types";
import { runGuarded } from "../guarded";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "./ui/Dialog";

interface AddSourceDialogProps {
  onAddText: () => Promise<void>;
  onAddImage: () => Promise<void>;
  onAddMedia: () => Promise<void>;
  onAddCandidate: (candidate: SourceCandidate) => Promise<void>;
  onEnumerate: () => Promise<SourceEnumeration>;
  onSelectPortal: () => Promise<SourceEnumeration>;
  onClose: () => void;
}

function AddSourceDialogImpl({
  onAddText,
  onAddImage,
  onAddMedia,
  onAddCandidate,
  onEnumerate,
  onSelectPortal,
  onClose,
}: AddSourceDialogProps) {
  const [candidates, setCandidates] = useState<SourceCandidate[]>([]);
  const [portalRequired, setPortalRequired] = useState(false);
  const [sourceLoading, setSourceLoading] = useState(false);
  const [actionBusy, setActionBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const operationBusyRef = useRef(false);
  const messageIsError = message != null && /nicht verfügbar|fehlgeschlagen|fehler/i.test(message);

  // Nur beim Mount enumerieren: das Dialog wird bei jeder Öffnung frisch
  // gerendert, daher genügt ein leerer Abhängigkeitsarray.
  useEffect(() => {
    let cancelled = false;
    setSourceLoading(true);
    onEnumerate()
      .then((result) => {
        if (cancelled) return;
        setCandidates(result.candidates);
        setPortalRequired(result.portalSelectionRequired);
        setMessage(result.message);
      })
      .catch((error: unknown) => {
        if (!cancelled) setMessage(String(error));
      })
      .finally(() => {
        if (!cancelled) setSourceLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Fängt Rejektionen der per onClick feuergelassenen Async-Flows ab und macht
  // sie über den Meldungskanal sichtbar (Helfer: src/guarded.ts).
  function guarded(flow: () => Promise<unknown>) {
    return () => {
      if (operationBusyRef.current) return;
      operationBusyRef.current = true;
      setActionBusy(true);
      void runGuarded(flow, setMessage).finally(() => {
        operationBusyRef.current = false;
        setActionBusy(false);
      });
    };
  }

  async function selectPortalSources() {
    if (operationBusyRef.current) return;
    operationBusyRef.current = true;
    setSourceLoading(true);
    try {
      const result = await onSelectPortal();
      setCandidates(result.candidates);
      setMessage(result.message);
      setPortalRequired(result.portalSelectionRequired);
    } catch (error) {
      setMessage(String(error));
    } finally {
      operationBusyRef.current = false;
      setSourceLoading(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !actionBusy && onClose()}>
      <DialogContent className="source-dialog" aria-describedby="add-source-description">
        <header className="modal-header">
          <div>
            <DialogTitle>Quelle hinzufügen</DialogTitle>
            <DialogDescription id="add-source-description">
              Inhalt erstellen, Datei öffnen oder laufende Quelle erfassen.
            </DialogDescription>
          </div>
          <button
            type="button"
            className="modal-close"
            aria-label="Quelle hinzufügen schließen"
            disabled={actionBusy}
            onClick={onClose}
          >
            ×
          </button>
        </header>

        <div className="source-picker">
          <section className="source-picker-section" aria-labelledby="content-source-heading">
            <h3 id="content-source-heading">Inhalt</h3>
            <div className="source-options">
              <button type="button" disabled={sourceLoading || actionBusy} onClick={guarded(onAddText)}>
                <strong>Text</strong><span>Beschriftung direkt in Hooviestar</span>
              </button>
              <button type="button" disabled={sourceLoading || actionBusy} onClick={guarded(onAddImage)}>
                <strong>Bild</strong><span>PNG, JPEG oder BMP</span>
              </button>
              <button type="button" disabled={sourceLoading || actionBusy} onClick={guarded(onAddMedia)}>
                <strong>Medium</strong><span>Video oder Audio aus MP4, MP3, WAV</span>
              </button>
            </div>
          </section>

          <section className="source-picker-section" aria-labelledby="capture-source-heading">
            <div className="section-heading-row">
              <h3 id="capture-source-heading">Bildschirm &amp; Audio</h3>
              {sourceLoading && <span role="status">Quellen werden gesucht …</span>}
              {actionBusy && <span role="status">Wird hinzugefügt …</span>}
            </div>
            <div className="source-options source-options-runtime">
              {portalRequired && (
                <button type="button" disabled={sourceLoading || actionBusy} onClick={() => void selectPortalSources()}>
                  <strong>Fenster oder Monitor auswählen</strong><span>Desktop-Portal öffnen</span>
                </button>
              )}
              {candidates.map((candidate) => (
                <button
                  type="button"
                  disabled={sourceLoading || actionBusy}
                  key={`${candidate.type}:${candidate.runtimeId}`}
                  onClick={guarded(() => onAddCandidate(candidate))}
                >
                  <strong>{candidate.name}</strong>
                  <span>{candidate.type === "window" ? "Fenster" : candidate.type === "display" ? "Monitor" : "Anwendungs-Audio"}</span>
                </button>
              ))}
              {!sourceLoading && !portalRequired && candidates.length === 0 && (
                <p className="source-picker-empty">Keine laufenden Quellen gefunden.</p>
              )}
            </div>
          </section>

          {message && (
            <p className={messageIsError ? "source-message" : "source-note"} role={messageIsError ? "alert" : "status"}>
              {message}
            </p>
          )}
        </div>

        <footer className="modal-actions">
          <button type="button" disabled={actionBusy} onClick={onClose}>Abbrechen</button>
        </footer>
      </DialogContent>
    </Dialog>
  );
}

export const AddSourceDialog = memo(AddSourceDialogImpl);
