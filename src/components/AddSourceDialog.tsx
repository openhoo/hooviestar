import { memo, useEffect, useRef, useState } from "react";
import type { SourceCandidate, SourceEnumeration } from "../types";
import { runGuarded } from "../guarded";

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
  const [message, setMessage] = useState<string | null>(null);
  const dialogRef = useRef<HTMLElement>(null);

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
    requestAnimationFrame(() => {
      dialogRef.current?.querySelector<HTMLElement>("button")?.focus();
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Fängt Rejektionen der per onClick feuergelassenen Async-Flows ab und macht
  // sie über den Meldungskanal sichtbar (Helfer: src/guarded.ts).
  function guarded(flow: () => Promise<unknown>) {
    return () => void runGuarded(flow, setMessage);
  }

  async function selectPortalSources() {
    setSourceLoading(true);
    try {
      const result = await onSelectPortal();
      setCandidates(result.candidates);
      setMessage(result.message);
      setPortalRequired(result.portalSelectionRequired);
    } catch (error) {
      setMessage(String(error));
    } finally {
      setSourceLoading(false);
    }
  }

  function trapDialogKeys(event: React.KeyboardEvent<HTMLElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const controls = Array.from(
      event.currentTarget.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)"),
    );
    if (controls.length === 0) return;
    const first = controls[0];
    const last = controls[controls.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  return (
    <div className="dialog-backdrop" role="presentation">
      <section ref={dialogRef} className="dialog" role="dialog" aria-modal="true" aria-labelledby="add-title" onKeyDown={trapDialogKeys}>
        <div className="panel-title"><h2 id="add-title">Quelle hinzufügen</h2><button aria-label="Schließen" onClick={onClose}>×</button></div>
        <div className="source-options">
          <button onClick={guarded(onAddText)}><strong>Text</strong><span>GPU-gerenderte Beschriftung</span></button>
          <button onClick={guarded(onAddImage)}><strong>Bild</strong><span>PNG, JPEG oder BMP</span></button>
          <button onClick={guarded(onAddMedia)}><strong>Medium</strong><span>MP4, MP3 oder WAV</span></button>
          {sourceLoading && <p role="status">Quellen werden gesucht…</p>}
          {portalRequired && <button onClick={() => void selectPortalSources()}><strong>Fenster oder Monitor auswählen</strong><span>Desktop-Portal öffnen</span></button>}
          {candidates.map((candidate) => (
            <button key={`${candidate.type}:${candidate.runtimeId}`} onClick={guarded(() => onAddCandidate(candidate))}>
              <strong>{candidate.name}</strong>
              <span>{candidate.type === "window" ? "Fenster" : candidate.type === "display" ? "Monitor" : "Anwendungs-Audio"}</span>
            </button>
          ))}
          {message && <p className="source-message">{message}</p>}
        </div>
      </section>
    </div>
  );
}

export const AddSourceDialog = memo(AddSourceDialogImpl);
