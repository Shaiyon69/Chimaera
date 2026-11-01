import { useTranslation } from "react-i18next";
import { LibraryMultiSelect } from "./library-multi-select";
import "./platform-filter.scss";

interface GenreFilterProps {
  selectedGenres: string[];
  genres: string[];
  onGenresChange: (genres: string[]) => void;
}

export function GenreFilter({
  selectedGenres,
  genres,
  onGenresChange,
}: Readonly<GenreFilterProps>) {
  const { t } = useTranslation("library");
  const allLabel = t("all_genres");

  const getTriggerLabel = () => {
    if (
      selectedGenres.length === 0 ||
      genres.every((genre) => selectedGenres.includes(genre))
    ) {
      return allLabel;
    }
    if (selectedGenres.length === 1) return selectedGenres[0];
    return t("selected_genres", { count: selectedGenres.length });
  };

  return (
    <div className="library-platform-filter__container">
      <LibraryMultiSelect
        value={selectedGenres}
        ariaLabel={allLabel}
        allLabel={allLabel}
        triggerLabel={getTriggerLabel()}
        onChange={onGenresChange}
        options={genres.map((genre) => ({ value: genre, label: genre }))}
      />
    </div>
  );
}
