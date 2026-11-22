/** Runs at most `max` tasks at once; the rest wait in call order. */
export const limitParallel = (max: number) => {
  let running = 0;
  const waiting: (() => void)[] = [];

  return async <T>(task: () => Promise<T>) => {
    if (running >= max) {
      // The finishing task hands its slot straight to us, so `running` is never released in between.
      await new Promise<void>((resolve) => waiting.push(resolve));
    } else {
      running += 1;
    }
    try {
      return await task();
    } finally {
      const next = waiting.shift();
      if (next) next();
      else running -= 1;
    }
  };
};
