import type { ShopAssets } from "@types";
import { cachedImage } from "@shared";
import { usePrefetchGame } from "@renderer/hooks/use-prefetch-game";

import "./game-card.scss";

export interface GameCardProps
  extends React.DetailedHTMLProps<
    React.ButtonHTMLAttributes<HTMLButtonElement>,
    HTMLButtonElement
  > {
  game: ShopAssets;
}

export function GameCard({ game, ...props }: GameCardProps) {
  const prefetch = usePrefetchGame(game.shop, game.objectId);

  return (
    <button
      {...props}
      type="button"
      className="game-card"
      onMouseEnter={(event) => {
        prefetch.onMouseEnter();
        props.onMouseEnter?.(event);
      }}
      onMouseLeave={(event) => {
        prefetch.onMouseLeave();
        props.onMouseLeave?.(event);
      }}
    >
      <div className="game-card__media">
        <img
          src={cachedImage(game.libraryImageUrl ?? undefined)}
          alt=""
          className="game-card__cover"
          loading="lazy"
        />
      </div>
      <div className="game-card__label">
        <p className="game-card__title">{game.title}</p>
      </div>
    </button>
  );
}
