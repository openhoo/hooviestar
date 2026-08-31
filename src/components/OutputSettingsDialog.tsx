import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";
import type { OutputConfig } from "../types";
import {
  isHexColor,
  isOutputFrameRate,
  OUTPUT_FRAME_RATES,
  OUTPUT_RESOLUTIONS,
  outputResolutionValue,
  parseOutputResolution,
  sameOutput,
} from "../outputSettings";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "./ui/Dialog";

interface OutputSettingsDialogProps {
  open: boolean;
  output: OutputConfig;
  onOpenChange: (open: boolean) => void;
  onApply: (output: OutputConfig) => Promise<void>;
}

export function OutputSettingsDialog({ open, output, onOpenChange, onApply }: OutputSettingsDialogProps) {
  const [draft, setDraft] = useState(output);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setDraft(output);
    setSaving(false);
    setError(null);
  }, [open, output.width, output.height, output.fps, output.background]);

  const normalizedDraft = useMemo<OutputConfig>(
    () => ({ ...draft, background: draft.background.toLowerCase() }),
    [draft],
  );
  const backgroundValid = isHexColor(draft.background);
  const dirty = backgroundValid && !sameOutput(normalizedDraft, output);

  function changeResolution(value: string) {
    const resolution = parseOutputResolution(value);
    if (resolution) setDraft((current) => ({ ...current, ...resolution }));
  }

  function changeFps(value: string) {
    const fps = Number(value);
    if (isOutputFrameRate(fps)) setDraft((current) => ({ ...current, fps }));
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!dirty || saving) return;
    setSaving(true);
    setError(null);
    try {
      await onApply(normalizedDraft);
      onOpenChange(false);
    } catch (cause) {
      setError(`Einstellungen konnten nicht angewendet werden: ${String(cause)}`);
      setSaving(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !saving && onOpenChange(next)}>
      <DialogContent className="settings-dialog" aria-describedby="output-settings-description">
        <header className="settings-header">
          <div>
            <DialogTitle>Ausgabe-Einstellungen</DialogTitle>
            <DialogDescription id="output-settings-description">
              Bestimmt Program-Ausgabe, Vorschau und neue Quellenlayouts.
            </DialogDescription>
          </div>
          <button
            type="button"
            className="settings-close"
            aria-label="Einstellungen schließen"
            disabled={saving}
            onClick={() => onOpenChange(false)}
          >
            ×
          </button>
        </header>

        <form className="settings-form" onSubmit={submit}>
          <fieldset className="settings-section" disabled={saving}>
            <legend>Videoausgabe</legend>
            <label htmlFor="output-resolution">
              <span>Auflösung</span>
              <select
                id="output-resolution"
                value={outputResolutionValue(draft)}
                onChange={(event) => changeResolution(event.currentTarget.value)}
              >
                {OUTPUT_RESOLUTIONS.map((resolution) => (
                  <option key={`${resolution.width}x${resolution.height}`} value={`${resolution.width}x${resolution.height}`}>
                    {resolution.label} — {resolution.description}
                  </option>
                ))}
              </select>
            </label>
            <label htmlFor="output-fps">
              <span>Bildrate</span>
              <select id="output-fps" value={draft.fps} onChange={(event) => changeFps(event.currentTarget.value)}>
                {OUTPUT_FRAME_RATES.map((fps) => (
                  <option key={fps} value={fps}>{fps} fps</option>
                ))}
              </select>
            </label>
            <p className="settings-help">
              Beim Auflösungswechsel skaliert Hooviestar alle Szenenelemente proportional. Sperren und Ebenenreihenfolge bleiben erhalten.
            </p>
          </fieldset>

          <fieldset className="settings-section" disabled={saving}>
            <legend>Leere Fläche</legend>
            <div className="color-setting">
              <label htmlFor="output-background">Hintergrundfarbe</label>
              <div>
                <input
                  type="color"
                  aria-label="Hintergrundfarbe auswählen"
                  value={backgroundValid ? draft.background : "#000000"}
                  onChange={(event) => {
                    const background = event.currentTarget.value;
                    setDraft((current) => ({ ...current, background }));
                  }}
                />
                <input
                  id="output-background"
                  type="text"
                  inputMode="text"
                  autoComplete="off"
                  spellCheck={false}
                  maxLength={7}
                  value={draft.background}
                  aria-invalid={!backgroundValid}
                  aria-describedby={!backgroundValid ? "output-background-error" : undefined}
                  onChange={(event) => {
                    const background = event.currentTarget.value;
                    setDraft((current) => ({ ...current, background }));
                  }}
                />
              </div>
              {!backgroundValid && (
                <p id="output-background-error" className="field-error" role="alert">
                  Farbe als sechsstelligen Hex-Wert eingeben, zum Beispiel #101418.
                </p>
              )}
            </div>
          </fieldset>

          <section className="settings-output-preview" aria-label="Vorschau der Ausgabeeinstellungen">
            <div
              className="settings-output-canvas"
              style={{
                aspectRatio: `${draft.width} / ${draft.height}`,
                backgroundColor: backgroundValid ? draft.background : "#000000",
              }}
            >
              <span>Program &amp; Vorschau</span>
            </div>
            <strong>{draft.width} × {draft.height} · {draft.fps} fps</strong>
          </section>

          {error && <p className="settings-error" role="alert">{error}</p>}

          <footer className="settings-actions">
            <button type="button" disabled={saving} onClick={() => onOpenChange(false)}>Abbrechen</button>
            <button type="submit" className="primary" disabled={!dirty || saving}>
              {saving ? "Wird angewendet …" : "Anwenden"}
            </button>
          </footer>
        </form>
      </DialogContent>
    </Dialog>
  );
}
