import { TrophyIcon } from "@primer/octicons-react";
import { isGameCompleted } from "@renderer/helpers";
import { ProgressBar } from "../progress-bar/progress-bar";

interface AchievementProgressProps {
  achievementCount: number;
  unlockedAchievementCount: number;
  classNamePrefix: string;
  label: string;
  trophyIconSize?: number;
}

export function AchievementProgress({
  achievementCount,
  unlockedAchievementCount,
  classNamePrefix,
  label,
  trophyIconSize = 13,
}: Readonly<AchievementProgressProps>) {
  const isCompleted = isGameCompleted(
    achievementCount,
    unlockedAchievementCount
  );
  // Total is unknown when the catalogue never loaded (0) or is stale (below the unlocks).
  const hasTotal =
    achievementCount >= unlockedAchievementCount && achievementCount > 0;
  const safeMax = hasTotal ? achievementCount : 1;
  const percentage = Math.round((unlockedAchievementCount / safeMax) * 100);

  const percentageClassName = `${classNamePrefix}__achievement-percentage`;
  const completedClassName = `${percentageClassName}--completed`;
  const className = isCompleted
    ? `${percentageClassName} ${completedClassName}`
    : percentageClassName;

  return (
    <div className={`${classNamePrefix}__achievements`}>
      <div className={`${classNamePrefix}__achievement-header`}>
        <div className={`${classNamePrefix}__achievements-gap`}>
          {!isCompleted && (
            <TrophyIcon
              size={trophyIconSize}
              className={`${classNamePrefix}__achievement-trophy`}
            />
          )}
          <span className={`${classNamePrefix}__achievement-count`}>
            {hasTotal
              ? `${unlockedAchievementCount} / ${achievementCount}`
              : unlockedAchievementCount}
          </span>
        </div>
        <span className={className}>
          {isCompleted && <TrophyIcon size={trophyIconSize} />}
          {!isCompleted && hasTotal && <>{percentage}%</>}
        </span>
      </div>
      <ProgressBar
        now={hasTotal ? unlockedAchievementCount : 0}
        max={safeMax}
        label={label}
        completed={isCompleted}
        trackClassName={`${classNamePrefix}__achievement-progress`}
        barClassName={`${classNamePrefix}__achievement-bar`}
      />
    </div>
  );
}
