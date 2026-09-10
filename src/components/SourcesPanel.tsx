import { Fragment, memo, useEffect, useRef, useState } from "react";
import type { SceneItem, Source } from "../types";
import type { ItemAction } from "./SourceInspectorPanel";
import { ConfirmDialog } from "./ConfirmDialog";
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
  affectedSceneNames: readonly string[];
  itemError: string | null;
  addButtonRef: React.RefObject<HTMLButtonElement | null>;
  onSelectSource: (sourceId: string) => void;
  onAddClick: () => void;
  onRemoveSource: (sourceId: string) => void | Promise<void>;
  onItemAction: (itemId: string, action: ItemAction) => void;
  onRemoveArmedChange?: (armed: boolean) => void;
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
  affectedSceneNames,
  itemError,
  addButtonRef,
  onSelectSource,
  onAddClick,
  onRemoveSource,
  onItemAction,
  onRemoveArmedChange,
}: SourcesPanelProps) {
  const minusButtonRef = useRef<HTMLButtonElement>(null);
  const [removeOpen, setRemoveOpen] = useState(false);
  const [removeSourceId, setRemoveSourceId] = useState<string | null>(null);
  const [removeBusy, setRemoveBusy] = useState(false);
  const removeBusyRef = useRef(false);
  const [removeError, setRemoveError] = useState<string | null>(null);

  useEffect(() => {
    onRemoveArmedChange?.(removeOpen);
  }, [onRemoveArmedChange, removeOpen]);
  useEffect(() => () => onRemoveArmedChange?.(false), [onRemoveArmedChange]);

  // Eine Auswahländerung darf keine Bestätigung auf eine andere Quelle
  // übertragen.
  useEffect(() => {
    if (removeOpen && removeSourceId !== selectedSourceId) {
      setRemoveOpen(false);
      setRemoveSourceId(null);
      setRemoveError(null);
    }
  }, [removeOpen, removeSourceId, selectedSourceId]);

  function requestRemove() {
    if (!selectedSourceId || removeBusyRef.current) return;
    setRemoveSourceId(selectedSourceId);
    setRemoveError(null);
    setRemoveOpen(true);
  }

  function cancelRemove() {
    if (removeBusyRef.current) return;
    setRemoveOpen(false);
    setRemoveSourceId(null);
    setRemoveError(null);
  }

  async function confirmRemove() {
    if (removeBusyRef.current) return;
    const targetId = removeSourceId;
    if (!targetId || targetId !== selectedSourceId || !rows.some((row) => row.source.id === targetId)) {
      setRemoveError("Die Quelle ist nicht mehr ausgewählt. Bitte die Entfernung erneut starten.");
      return;
    }
    removeBusyRef.current = true;
    setRemoveBusy(true);
    setRemoveError(null);
    try {
      await onRemoveSource(targetId);
      setRemoveOpen(false);
      setRemoveSourceId(null);
    } catch (error) {
      setRemoveError(String(error));
    } finally {
      removeBusyRef.current = false;
      setRemoveBusy(false);
    }
  }

  function handleRemoveButtonClick() {
    if (removeOpen) void confirmRemove();
    else requestRemove();
  }

  function handleDockKeyDown(event: React.KeyboardEvent<HTMLElement>) {
    if (
      event.defaultPrevented ||
      (event.key !== "Delete" && event.key !== "Backspace") ||
      !(event.target instanceof HTMLElement) ||
      event.target.isContentEditable ||
      event.target.tagName === "INPUT" ||
      event.target.tagName === "TEXTAREA" ||
      event.target.tagName === "SELECT" ||
      event.target.closest('[role="dialog"], [aria-modal="true"], .modal-dialog') ||
      !selectedSourceId
    ) {
      return;
    }
    const target = event.target;
    const selectedRow = target.closest(".source-row.selected");
    const removeControl = target.closest('[data-delete-trigger="source"]');
    if (!selectedRow && !removeControl) return;
    event.preventDefault();
    event.stopPropagation();
    handleRemoveButtonClick();
  }

  const removeClassName = removeOpen ? "icon-button armed" : "icon-button";
  const removeTitle = removeOpen ? "Erneut klicken zum Entfernen" : "Ausgewählte Quelle entfernen";
  const selectedSourceName = rows.find((row) => row.source.id === removeSourceId)?.source.name
    ?? rows.find((row) => row.source.id === selectedSourceId)?.source.name
    ?? "Quelle";
  const firstUnplacedIndex = rows.findIndex((row) => !row.itemId);
  const placedCount = firstUnplacedIndex === -1 ? rows.length : firstUnplacedIndex;
  const unplacedCount = rows.length - placedCount;

  return (
    <section className="dock sources-dock" aria-label="Quellen" aria-keyshortcuts="Delete" onKeyDown={handleDockKeyDown}>
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
            aria-keyshortcuts="Delete"
            aria-expanded={removeOpen}
            title={removeTitle}
            data-delete-trigger="source"
            disabled={!selectedSourceId}
            onClick={handleRemoveButtonClick}
          >
            <MinusIcon />
          </button>
        </div>
      </div>
      {rows.length === 0 ? (
        <p className="empty">Keine Quellen in dieser Szene.</p>
      ) : (
        <ul className="sources-list">
          {rows.map((row, index) => {
            const { source, itemId, visible, locked, canMoveUp, canMoveDown } = row;
            const selected = source.id === selectedSourceId;
            return (
              <Fragment key={row.key}>
                {index === firstUnplacedIndex && (
                  <li className="source-list-divider">
                    <span>{placedCount > 0 ? "Weitere Quellen" : "Nicht in dieser Szene"}</span>
                    <span>{unplacedCount}</span>
                  </li>
                )}
                <li>
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
                    {itemId && (
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
                    )}
                  </div>
                </li>
              </Fragment>
            );
          })}
        </ul>
      )}
      {itemError && <p className="dock-message" role="alert">{itemError}</p>}
      <ConfirmDialog
        open={removeOpen}
        title={`Quelle „${selectedSourceName}“ entfernen?`}
        description="Die Quelle wird global entfernt und aus allen Szenen gelöscht, in denen sie verwendet wird."
        details={affectedSceneNames.length > 0 ? (
          <>
            <span>Betroffene Szenen:</span>
            <ul>
              {affectedSceneNames.map((name, index) => <li key={`${index}:${name}`}>{name}</li>)}
            </ul>
          </>
        ) : (
          <span>Diese Quelle ist keiner Szene zugeordnet.</span>
        )}
        error={removeError}
        busy={removeBusy}
        onCancel={cancelRemove}
        onConfirm={confirmRemove}
        onRestoreFocus={() => minusButtonRef.current?.focus()}
      />
    </section>
  );
}

export const SourcesPanel = memo(SourcesPanelImpl);
