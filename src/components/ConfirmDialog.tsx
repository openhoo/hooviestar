import { useId, useRef } from "react";
import type { ReactNode } from "react";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "./ui/Dialog";

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  description: ReactNode;
  details?: ReactNode;
  error: string | null;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void | Promise<void>;
  onRestoreFocus: () => void;
}

export function ConfirmDialog({
  open,
  title,
  description,
  details,
  error,
  busy,
  onCancel,
  onConfirm,
  onRestoreFocus,
}: ConfirmDialogProps) {
  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  const descriptionId = useId();
  const detailsId = useId();

  return (
    <Dialog open={open} onOpenChange={(next) => !next && !busy && onCancel()}>
      <DialogContent
        className="confirm-dialog"
        role="alertdialog"
        aria-describedby={details ? `${descriptionId} ${detailsId}` : descriptionId}
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          cancelButtonRef.current?.focus();
        }}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          onRestoreFocus();
        }}
      >
        <header className="modal-header">
          <div>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription id={descriptionId}>{description}</DialogDescription>
          </div>
        </header>
        {details && <div id={detailsId} className="confirm-details">{details}</div>}
        {error && <p className="settings-error" role="alert">{error}</p>}
        <footer className="modal-actions">
          <button ref={cancelButtonRef} type="button" disabled={busy} onClick={onCancel}>
            Abbrechen
          </button>
          <button type="button" className="primary danger-action" disabled={busy} onClick={() => void onConfirm()}>
            {busy ? "Wird entfernt …" : "Entfernen"}
          </button>
        </footer>
      </DialogContent>
    </Dialog>
  );
}
