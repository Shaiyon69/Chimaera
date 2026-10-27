/** At most one "typing" broadcast per this long while the user keeps typing. */
export const TYPING_SEND_MS = 3000;

/** The indicator clears this long after the last signal, since "stopped" is never sent. */
export const TYPING_SHOW_MS = 5000;

/** Returns a gate: true when a signal may go out now, false while still inside the interval. */
export const createThrottle = (
  intervalMs: number,
  now: () => number = Date.now
) => {
  let last = -Infinity;

  return () => {
    const t = now();
    if (t - last < intervalMs) return false;

    last = t;
    return true;
  };
};
