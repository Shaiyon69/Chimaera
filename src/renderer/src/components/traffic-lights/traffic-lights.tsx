import { useTranslation } from "react-i18next";
import "./traffic-lights.scss";

// Drawn strokes, not text glyphs: font metrics sit "−", "+" and "×" off the dot's centre.
const glyph = (path: string) => (
  <svg viewBox="0 0 8 8" aria-hidden="true">
    <path d={path} />
  </svg>
);

interface TrafficLightsProps {
  isMaximized?: boolean;
}

export function TrafficLights({ isMaximized = false }: TrafficLightsProps) {
  const { t } = useTranslation("header");
  const maximizeLabel = isMaximized ? t("restore") : t("maximize");

  // Each dot carries its symbol: red/green alone cannot be told apart by red-green colourblind users.
  return (
    <div className="traffic-lights">
      <button
        type="button"
        className="traffic-lights__min"
        title={t("minimize")}
        aria-label={t("minimize")}
        onClick={() => window.electron.minimizeMainWindow()}
      >
        {glyph("M1.5 4h5")}
      </button>
      <button
        type="button"
        className="traffic-lights__max"
        title={maximizeLabel}
        aria-label={maximizeLabel}
        onClick={() => window.electron.toggleMaximizeMainWindow()}
      >
        {glyph("M1.5 4h5M4 1.5v5")}
      </button>
      <button
        type="button"
        className="traffic-lights__close"
        title={t("close")}
        aria-label={t("close")}
        onClick={() => window.electron.closeMainWindow()}
      >
        {glyph("M2 2l4 4M6 2l-4 4")}
      </button>
    </div>
  );
}
