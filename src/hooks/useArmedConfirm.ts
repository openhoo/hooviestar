import { useCallback, useEffect, useRef, useState } from "react";

type TriggerRef = React.RefObject<HTMLElement | null>;

/**
 * Geteilte Zwei-Klick-Entfernung: der erste Klick armiert den Auslöser, der
 * zweite bestätigt. Die Armierung verfällt, wenn sich der Kontext ändert
 * (`disarmKey`), bei pointerdown außerhalb aller Auslöser und bei Escape.
 * Während der laufenden Bestätigung ignoriert der Auslöser weitere Klicks
 * (busyRef vom Bestätigungsstart bis zum Settlen des Flows).
 */
export function useArmedConfirm(
  onConfirm: () => void | Promise<void>,
  disarmKey: string | null,
  triggerRefs: readonly TriggerRef[],
): { armed: boolean; trigger: () => void } {
  const [armed, setArmed] = useState(false);
  const busyRef = useRef(false);

  // Kontextwechsel (z. B. Auswahl- oder Szenenwechsel) entschärft eine
  // offene Bestätigung.
  useEffect(() => {
    setArmed(false);
  }, [disarmKey]);

  // pointerdown außerhalb aller Auslöser entschärft die Bestätigung wieder.
  useEffect(() => {
    if (!armed) return;
    const disarmOutside = (event: PointerEvent) => {
      if (!(event.target instanceof Node)) return;
      if (triggerRefs.some((ref) => ref.current?.contains(event.target as Node))) return;
      setArmed(false);
    };
    document.addEventListener("pointerdown", disarmOutside);
    return () => document.removeEventListener("pointerdown", disarmOutside);
  }, [armed, triggerRefs]);

  // Escape entschärft ebenfalls.
  useEffect(() => {
    if (!armed) return;
    const disarmOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setArmed(false);
    };
    document.addEventListener("keydown", disarmOnEscape);
    return () => document.removeEventListener("keydown", disarmOnEscape);
  }, [armed]);

  const trigger = useCallback(() => {
    if (!armed) {
      setArmed(true);
      return;
    }
    if (busyRef.current) return;
    setArmed(false);
    busyRef.current = true;
    // Der Flow läuft asynchron an; ein synchroner Wurf wird abgefangen und
    // busy in jedem Fall freigegeben. Rejektionen hat der Flow selbst
    // sichtbar gemacht (runGuarded), hier nur Unbehandeltsein vermeiden.
    void Promise.resolve()
      .then(onConfirm)
      .catch(() => undefined)
      .finally(() => {
        busyRef.current = false;
      });
  }, [armed, onConfirm]);

  return { armed, trigger };
}
