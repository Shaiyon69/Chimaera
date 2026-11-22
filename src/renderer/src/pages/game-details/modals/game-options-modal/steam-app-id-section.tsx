import { useContext, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, TextField } from "@renderer/components";
import { gameDetailsContext } from "@renderer/context";
import { useToast } from "@renderer/hooks";
import type { LibraryGame } from "@types";

export interface SteamAppIdSectionProps {
  game: LibraryGame;
}

/** Ties a custom game to a Steam appid so Chimaera can read its achievements. */
export function SteamAppIdSection({ game }: Readonly<SteamAppIdSectionProps>) {
  const { t } = useTranslation("game_details");
  const { updateGame } = useContext(gameDetailsContext);
  const { showSuccessToast, showErrorToast } = useToast();
  const [value, setValue] = useState(game.steamAppId ?? "");
  const [isSaving, setIsSaving] = useState(false);

  const save = async (input: string | null) => {
    setIsSaving(true);
    try {
      const saved = await window.electron.updateGameSteamAppId(
        game.shop,
        game.objectId,
        input
      );
      setValue(saved ?? "");
      await updateGame();
      showSuccessToast(
        saved ? t("steam_app_id_saved") : t("steam_app_id_cleared")
      );
    } catch {
      showErrorToast(t("steam_app_id_invalid"));
    } finally {
      setIsSaving(false);
    }
  };

  const isUnchanged = value.trim() === (game.steamAppId ?? "");

  return (
    <div className="game-options-modal__launch-options">
      <div className="game-options-modal__header">
        <h2>{t("steam_app_id")}</h2>
        <h4 className="game-options-modal__header-description">
          {t("steam_app_id_description")}
        </h4>
      </div>
      <TextField
        value={value}
        theme="dark"
        placeholder={t("steam_app_id_placeholder")}
        onChange={(event) => setValue(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && !isUnchanged && !isSaving) save(value);
        }}
        rightContent={
          <>
            <Button
              onClick={() => save(value)}
              theme="outline"
              disabled={isSaving || isUnchanged}
            >
              {t("steam_app_id_save")}
            </Button>
            {game.steamAppId && (
              <Button
                onClick={() => save(null)}
                theme="outline"
                disabled={isSaving}
              >
                {t("clear")}
              </Button>
            )}
          </>
        }
      />
    </div>
  );
}
