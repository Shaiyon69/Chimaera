import { useEffect, useRef } from "react";

// Start loading the next page this far before the bottom.
const NEAR_BOTTOM_PX = 400;

/**
 * Infinite scroll on the element with id `scrollTargetId`: loads the next page when the
 * user scrolls near the bottom, and keeps loading while the list is too short to scroll.
 * Every scroll re-checks, so a load skipped while another was running (or a page that
 * added nothing) never leaves the list stuck. Without scrolling it only re-checks when
 * the item count changes, so a failed load does not retry in a loop.
 */
export function useFillScrollTarget(
  scrollTargetId: string,
  dataLength: number,
  hasMore: boolean,
  loadMore: () => unknown,
  isLoading = false
) {
  const state = useRef({ hasMore, loadMore, isLoading });
  state.current = { hasMore, loadMore, isLoading };

  useEffect(() => {
    const el = document.getElementById(scrollTargetId);
    if (!el) return;

    const check = () => {
      const { hasMore, loadMore, isLoading } = state.current;
      if (!hasMore || isLoading) return;
      if (el.scrollTop + el.clientHeight >= el.scrollHeight - NEAR_BOTTOM_PX) {
        loadMore();
      }
    };

    el.addEventListener("scroll", check, { passive: true });
    // Wait a frame so newly rendered items are laid out before measuring.
    const frame = dataLength > 0 ? requestAnimationFrame(check) : 0;
    return () => {
      el.removeEventListener("scroll", check);
      cancelAnimationFrame(frame);
    };
  }, [scrollTargetId, dataLength, hasMore]);
}
