import { Sparkle } from "lucide-react";
import "./art-fallback.scss";

export interface ArtFallbackProps {
  title: string;
  /** "cover" for portrait cards, "banner" for wide hero art. */
  variant?: "cover" | "banner";
  className?: string;
}

/** Tarot-style stand-in shown when a game has no art, or all of its art fails to load. */
export function ArtFallback({
  title,
  variant = "cover",
  className,
}: Readonly<ArtFallbackProps>) {
  return (
    <div
      role="img"
      aria-label={title}
      className={["art-fallback", `art-fallback--${variant}`, className]
        .filter(Boolean)
        .join(" ")}
    >
      <div className="art-fallback__frame" aria-hidden="true">
        <Sparkle className="art-fallback__star" strokeWidth={1.75} />
        <span className="art-fallback__title">{title}</span>
        <span className="art-fallback__rule" />
        <span className="art-fallback__brand">Chimaera</span>
      </div>
    </div>
  );
}
