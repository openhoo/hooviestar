import { memo, useMemo, useRef } from "react";
import type { SceneItem, Source } from "../types";
import type { ItemAction } from "./SourceInspectorPanel";
import { useArmedConfirm } from "../hooks/useArmedConfirm";
import {
  ArrowDownIcon,
  ArrowUpIcon,
  EyeIcon,
  EyeOffIcon,
  LockIcon,
  MinusIcon,
  PlusIcon,
  UnlockIcon,
} from "./icons";

export interface SourceRow {
  key: string;
  source: Source;
  itemId?: string;
  visible?: boolean;
  locked?: boolean;
  canMoveUp?: boolean;
  canMoveDown?: boolean;
}

/** Engine-Reihenfolge unten-nach-oben als bedienbare Layerliste oben-nach-unten. */
export function sourceRowsFor(sources: Source[], items: SceneItem[]): SourceRow[] {
  const sourcesById = new Map(sources.map((source) => [source.id, source]));
  const placedSourceIds = new Set(items.map((item) => item.sourceId));
  const placed = items.map<SourceRow>((item, index) => {
    const source = sourcesById.get(item.sourceId);
    if (!source) throw new Error(`Quelle ${item.sourceId} für Szenenelement fehlt`);
    return {
      key: item.id,
      source,
      itemId: item.id,
      visible: item.visible,
      locked: item.locked,
      canMoveUp: !item.locked && index < items.length - 1,
      canMoveDown: !item.locked && index > 0,
    };
  });
  const unplaced: SourceRow[] = sources
    .filter((source) => !placedSourceIds.has(source.id))
    .map((source) => ({ key: source.id, source }));
  return [...placed.reverse(), ...unplaced];
}

interface SourcesPanelProps {
  rows: SourceRow[];
  selectedSourceId: string | null;
  itemError: string | null;
  addButtonRef: React.RefObject<HTMLButtonElement | null>;
  onSelectSource: (sourceId: string) => void;
  onAddClick: () => void;
  onRemoveSource: (sourceId: string) => void | Promise<void>;
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
  itemError,
  addButtonRef,
  onSelectSource,
  onAddClick,
  onRemoveSource,
  onItemAction,
}: SourcesPanelProps) {
  const minusButtonRef = useRef<HTMLButtonElement>(null);
  const removeTriggerRefs = useMemo(() => [minusButtonRef], []);
  // Geteilte Zwei-Klick-Entfernung: Auswahlwechsel, Klick außerhalb und
  // Escape entschärfen; während der Bestätigung werden weitere Klicks ignoriert.
  const { armed, trigger: handleRemoveClick } = useArmedConfirm(() => {
    if (selectedSourceId) onRemoveSource(selectedSourceId);
  }, selectedSourceId, removeTriggerRefs);

  const removeClassName = armed ? "icon-button armed" : "icon-button";
  const removeTitle = armed ? "Erneut klicken zum Entfernen" : "Ausgewählte Quelle entfernen";

  return (
    <section className="dock sources-dock" aria-label="Quellen">
      <div className="dock-title">
        <div className="dock-heading">
          <h2>Quellen</h2>
          <span>{rows.length}</span>
        </div>
        <div className="dock-toolbar">
          <button
            ref={addButtonRef}
            type="button"
            className="icon-button"
            aria-label="Quelle hinzufügen"
            title="Quelle hinzufügen"
            onClick={onAddClick}
          >
            <PlusIcon />
          </button>
          <button
            ref={minusButtonRef}
            type="button"
            className={removeClassName}
            aria-label={removeTitle}
            title={removeTitle}
            disabled={!selectedSourceId}
            onClick={handleRemoveClick}
          >
            <MinusIcon />
          </button>
        </div>
      </div>
      {rows.length === 0 ? (
        <p className="empty">Keine Quellen in dieser Szene.</p>
      ) : (
        <ul className="sources-list">
          {rows.map((row) => {
            const { source, itemId, visible, locked, canMoveUp, canMoveDown } = row;
            const selected = source.id === selectedSourceId;
            return (
              <li key={row.key}>
                <div className={selected ? "source-row selected" : "source-row"}>
                  <button
                    type="button"
                    className="source-main"
                    aria-current={selected ? "true" : undefined}
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
                        title={locked ? "Gesperrte Quelle kann nicht verschoben werden" : canMoveUp ? "Nach oben" : "Bereits ganz oben"}
                        disabled={!canMoveUp}
                        onClick={() => onItemAction(itemId, "moveUp")}
                      >
                        <ArrowUpIcon />
                      </button>
                      <button
                        type="button"
                        className="icon-button"
                        aria-label="Nach unten"
                        title={locked ? "Gesperrte Quelle kann nicht verschoben werden" : canMoveDown ? "Nach unten" : "Bereits ganz unten"}
                        disabled={!canMoveDown}
                        onClick={() => onItemAction(itemId, "moveDown")}
                      >
                        <ArrowDownIcon />
                      </button>
                    </span>
                  ) : (
                    <span className="badge">Nicht in Szene</span>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
      {itemError && <p className="dock-message" role="alert">{itemError}</p>}
    </section>
  );
}

export const SourcesPanel = memo(SourcesPanelImpl);
