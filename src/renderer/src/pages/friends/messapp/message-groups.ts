/** Messages from one sender closer together than this read as a single group. */
export const GROUP_GAP_MS = 5 * 60_000;

interface GroupableLine {
  id: string;
  mine: boolean;
  createdAt: string;
}

export type ChatItem<L extends GroupableLine> =
  | { kind: "day"; key: string; createdAt: string }
  | { kind: "line"; line: L; startsGroup: boolean; endsGroup: boolean };

const dayKey = (iso: string) => {
  const d = new Date(iso);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
};

/** Whole calendar days between `iso` and `now` in local time: 0 today, 1 yesterday. */
export const dayDiff = (iso: string, now: Date = new Date()) => {
  const midnight = (d: Date) =>
    new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

  return Math.round(
    (midnight(now) - midnight(new Date(iso))) / (24 * 60 * 60_000)
  );
};

const continues = <L extends GroupableLine>(prev: L, next: L) =>
  prev.mine === next.mine &&
  dayKey(prev.createdAt) === dayKey(next.createdAt) &&
  new Date(next.createdAt).getTime() - new Date(prev.createdAt).getTime() <=
    GROUP_GAP_MS;

/** Interleaves a day separator before the first message of each day and marks group edges. */
export const groupLines = <L extends GroupableLine>(
  lines: L[]
): ChatItem<L>[] =>
  lines.flatMap((line, i) => {
    const prev = lines[i - 1];
    const next = lines[i + 1];
    const items: ChatItem<L>[] = [];

    if (!prev || dayKey(prev.createdAt) !== dayKey(line.createdAt)) {
      items.push({
        kind: "day",
        key: `day-${dayKey(line.createdAt)}`,
        createdAt: line.createdAt,
      });
    }

    items.push({
      kind: "line",
      line,
      startsGroup: !prev || !continues(prev, line),
      endsGroup: !next || !continues(line, next),
    });
    return items;
  });
