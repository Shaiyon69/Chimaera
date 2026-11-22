import { useTranslation } from "react-i18next";
import { useContext } from "react";
import { format } from "date-fns";
import { getDateLocale } from "@shared";
import { gameDetailsContext } from "@renderer/context";
import {
  Boxes,
  Car,
  Castle,
  Clapperboard,
  Compass,
  Cog,
  Crosshair,
  EyeOff,
  FlaskConical,
  Footprints,
  Gamepad2,
  Ghost,
  Gift,
  Music,
  Palette,
  Puzzle,
  Shield,
  Skull,
  Sparkles,
  Swords,
  Tag,
  Tent,
  Trophy,
  Users,
  Wrench,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { genreKind, type GenreKind } from "./genre-kind";
import "./description-header.scss";

const GENRE_ICONS: Record<GenreKind, LucideIcon> = {
  action: Swords,
  strategy: Castle,
  rpg: Shield,
  casual: Gamepad2,
  racing: Car,
  sports: Trophy,
  indie: Sparkles,
  adventure: Compass,
  simulation: Cog,
  multiplayer: Users,
  free: Gift,
  early_access: FlaskConical,
  violent: Skull,
  nudity: EyeOff,
  horror: Ghost,
  puzzle: Puzzle,
  platformer: Footprints,
  shooter: Crosshair,
  fighting: Zap,
  sandbox: Boxes,
  survival: Tent,
  music: Music,
  tools: Wrench,
  creative: Palette,
  video: Clapperboard,
  other: Tag,
};

const ISO_DATE_REGEX = /^\d{4}-\d{2}-\d{2}$/;

export function formatReleaseDate(rawDate: string, language: string) {
  if (!ISO_DATE_REGEX.test(rawDate)) return rawDate;

  const parsed = new Date(`${rawDate}T00:00:00`);
  if (isNaN(parsed.getTime())) return rawDate;

  return format(parsed, "MMM d, yyyy", { locale: getDateLocale(language) });
}

export function DescriptionHeader() {
  const { shopDetails } = useContext(gameDetailsContext);
  const { t, i18n } = useTranslation("game_details");

  if (!shopDetails) return null;

  const displayDate = formatReleaseDate(
    shopDetails?.release_date.date ?? "",
    i18n.language
  );

  const developers = shopDetails.developers?.join(", ");
  const publishers = shopDetails.publishers?.join(", ");

  // Self-published games list the same studio twice.
  const facts = [
    { label: t("developer_label"), value: developers },
    {
      label: t("publisher_label"),
      value: publishers === developers ? "" : publishers,
    },
    { label: t("released_label"), value: displayDate },
  ].filter((fact) => fact.value);

  // Steam sends { id, description }; other shops are mapped to { id, name }.
  const rawGenres = shopDetails.genres ?? [];
  const fromSteam = rawGenres.some((genre) => genre.description);
  const genres = rawGenres
    .map((genre) => ({
      id: genre.id,
      label: genre.name || genre.description,
      Icon: GENRE_ICONS[genreKind(genre, fromSteam)],
    }))
    .filter((genre) => genre.label);

  return (
    <div className="description-header">
      {facts.length > 0 && (
        <dl className="description-header__facts">
          {facts.map((fact) => (
            <div key={fact.label} className="description-header__fact">
              <dt>{fact.label}</dt>
              <dd>{fact.value}</dd>
            </div>
          ))}
        </dl>
      )}

      {genres.length > 0 && (
        <ul className="description-header__genres" aria-label={t("genres")}>
          {genres.map((genre) => (
            <li key={genre.id} className="description-header__genre">
              <genre.Icon size={13} strokeWidth={2.2} aria-hidden="true" />
              {genre.label}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
