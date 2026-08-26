import { memo, useEffect, useRef, useState } from "react";
import type { Source } from "../types";
import type { ItemAction } from "./SourceInspectorPanel";
import {
  ArrowDownIcon,
  ArrowUpIcon,
  EyeIcon,
  EyeOffIcon,
  LockIcon,
  MinusIcon,
  PlusIcon,
  TrashIcon,
  UnlockIcon,
} from "./icons";

export interface SourceRow {
  key: string;
  source: Source;
  itemId?: string;
  visible?: boolean;
  locked?: boolean;
}

interface SourcesPanelProps {
  rows: SourceRow[];
  selectedSourceId: string | null;
  addButtonRef: React.RefObject<HTMLButtonElement | null>;
  onSelectSource: (sourceId: string) => void;
  onAddClick: () => void;
  onRemoveSource: (sourceId: string) => void;
  onItemAction: (itemId: string, action: ItemAction) => void;
}

/** Generisches Quellen-Kästchen (Rechteck + Bildlinien), 16px. */
function SourceGlyph() {
  return (
    <svg
      viewBox="0 0 16 16"
      width={16}
      height={16}
      aria-hidden="true"
      focusable="false"
      stroke="currentColor"
      strokeWidth={1.5}
      fill="none"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <rect x="2.5" y="3.5" width="11" height="9" rx="1" />
      <path d="M2.5 6.2h11M6 3.5v2.7" />
    </svg>
  );
}

function SourcesPanelImpl({
  rows,
  selectedSourceId,
  addButtonRef,
  onSelectSource,
  onAddClick,
  onRemoveSource,
  onItemAction,
}: SourcesPanelProps) {
  const [armed, setArmed] = useState(false);
  const minusButtonRef = useRef<HTMLButtonElement>(null);
  const trashButtonRef = useRef<HTMLButtonElement>(null);

  // Auswahlwechsel entschärft eine offene Löschbestätigung.
  useEffect(() => {
    setArmed(false);
  }, [selectedSourceId]);

  // Klick außerhalb der Entfernen-Knöpfe entschärft die Bestätigung wieder.
  useEffect(() => {
    if (!armed) return;
    const disarmOutside = (event: PointerEvent) => {
      if (!(event.target instanceof Node)) return;
      if (
        minusButtonRef.current?.contains(event.target) ||
        trashButtonRef.current?.contains(event.target)
      ) {
        return;
      }
      setArmed(false);
    };
    window.addEventListener("pointerdown", disarmOutside);
    return () => window.removeEventListener("pointerdown", disarmOutside);
  }, [armed]);

  const handleRemoveClick = () => {
    if (!selectedSourceId) return;
    if (!armed) {
      setArmed(true);
      return;
    }
    setArmed(false);
    onRemoveSource(selectedSourceId);
  };

  const removeClassName = armed ? "icon-button armed" : "icon-button";
  const removeTitle = armed ? "Erneut klicken zum Entfernen" : "Ausgewählte Quelle entfernen";

  return (
    <section className="dock sources-dock" aria-label="Quellen">
      <div className="dock-title">
        <h2>Quellen</h2>
        <div className="dock-toolbar">
          <button
            ref={addButtonRef}
            type="button"
            className="icon-button"
            title="Quelle hinzufügen"
            onClick={onAddClick}
          >
            <PlusIcon />
          </button>
          <button
            ref={minusButtonRef}
            type="button"
            className={removeClassName}
            title={removeTitle}
            disabled={!selectedSourceId}
            onClick={handleRemoveClick}
          >
            <MinusIcon />
          </button>
          <button
            ref={trashButtonRef}
            type="button"
            className={removeClassName}
            title={removeTitle}
            disabled={!selectedSourceId}
            onClick={handleRemoveClick}
          >
            <TrashIcon />
          </button>
        </div>
      </div>
      {rows.length === 0 ? (
        <p className="empty">Keine Quellen in dieser Szene.</p>
      ) : (
        <ul className="sources-list">
          {rows.map((row) => {
            const { source, itemId, visible, locked } = row;
            const selected = source.id === selectedSourceId;
            return (
              <li key={row.key}>
                <div className={selected ? "source-row selected" : "source-row"}>
                  <button
                    type="button"
                    className="source-main"
                    onClick={() => onSelectSource(source.id)}
                  >
                    <span className="source-glyph">
                      <SourceGlyph />
                    </span>
                    <span className="source-name" title={source.name}>
                      {source.name}
                    </span>
                  </button>
                  {itemId ? (
                    <span className="row-actions">
                      <button
                        type="button"
                        className="icon-button"
                        aria-label={visible ? "Ausblenden" : "Einblenden"}
                        title={visible ? "Ausblenden" : "Einblenden"}
                        onClick={() => onItemAction(itemId, "toggleVisible")}
                      >
                        {visible ? <EyeIcon /> : <EyeOffIcon />}
                      </button>
                      <button
                        type="button"
                        className="icon-button"
                        aria-label={locked ? "Entsperren" : "Sperren"}
                        title={locked ? "Entsperren" : "Sperren"}
                        onClick={() => onItemAction(itemId, "toggleLocked")}
                      >
                        {locked ? <LockIcon /> : <UnlockIcon />}
                      </button>
                      <button
                        type="button"
                        className="icon-button"
                        aria-label="Nach oben"
                        title="Nach oben"
                        onClick={() => onItemAction(itemId, "moveUp")}
                      >
                        <ArrowUpIcon />
                      </button>
                      <button
                        type="button"
                        className="icon-button"
                        aria-label="Nach unten"
                        title="Nach unten"
                        onClick={() => onItemAction(itemId, "moveDown")}
                      >
                        <ArrowDownIcon />
                      </button>
                    </span>
                  ) : (
                    <span className="badge">außerhalb der Szene</span>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

export const SourcesPanel = memo(SourcesPanelImpl);
