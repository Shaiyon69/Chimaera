import { type RefObject, useLayoutEffect } from "react";

const positions = new Map<string, number>();
// Lists load after navigation, so the saved offset may not fit yet.
const RESTORE_TIMEOUT_MS = 3000;
const INTERRUPT_EVENTS = ["wheel", "touchstart", "keydown", "pointerdown"];

/**
 * Remembers an element's scroll offset under `key` and puts it back when that key is
 * active again. `key` null means the element is hidden: nothing is saved or restored.
 * With `restore` false the element starts at the top instead.
 */
export function useScrollRestoration(
  ref: RefObject<HTMLElement | null>,
  key: string | null,
  restore = true
) {
  // Layout effect: the old key's listener must detach before the browser clamps or
  // resets the offset for the next page, or that 0 would overwrite the saved value.
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element || key === null) return;

    const target = restore ? (positions.get(key) ?? 0) : 0;
    const stopAt = performance.now() + RESTORE_TIMEOUT_MS;
    let frame = 0;

    const apply = () => {
      element.scrollTo({ top: target, behavior: "instant" });
      const reached = element.scrollTop >= target - 1;
      frame =
        reached || performance.now() > stopAt
          ? 0
          : requestAnimationFrame(apply);
    };
    const stopRestoring = () => {
      cancelAnimationFrame(frame);
      frame = 0;
    };
    const save = () => {
      if (frame === 0) positions.set(key, element.scrollTop);
    };

    apply();
    element.addEventListener("scroll", save, { passive: true });
    for (const type of INTERRUPT_EVENTS) {
      element.addEventListener(type, stopRestoring, { passive: true });
    }

    return () => {
      stopRestoring();
      element.removeEventListener("scroll", save);
      for (const type of INTERRUPT_EVENTS) {
        element.removeEventListener(type, stopRestoring);
      }
    };
  }, [ref, key, restore]);
}
