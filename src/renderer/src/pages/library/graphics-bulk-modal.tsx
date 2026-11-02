import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  Button,
  CheckboxField,
  Modal,
  SelectField,
} from "@renderer/components";
import {
  profileName,
  type OptiscalerProfile,
} from "@renderer/pages/game-details/modals/game-options-modal/optiscaler-profiles";
import { VersionSelect } from "@renderer/pages/game-details/modals/game-options-modal/version-select";
import { ConfirmBar } from "@renderer/pages/game-details/modals/game-options-modal/graphics-ui";
import { findBundledGame } from "@shared";
import type { LibraryGame } from "@types";

import "./graphics-bulk-modal.scss";

type Tool = "optiscaler" | "dxvk";
type Build = "gplasync" | "dxvk";
type Result = { state: "running" | "ok" | "skipped" | "failed"; text: string };

const keyOf = (g: LibraryGame) => `${g.shop}:${g.objectId}`;
const errorText = (err: unknown) =>
  err instanceof Error ? err.message : String(err);

export function GraphicsBulkModal({
  visible,
  games,
  onClose,
}: Readonly<{ visible: boolean; games: LibraryGame[]; onClose: () => void }>) {
  const { t } = useTranslation("library");
  const [tool, setTool] = useState<Tool>("optiscaler");
  const [build, setBuild] = useState<Build>("gplasync");
  const [version, setVersion] = useState("");
  const [profiles, setProfiles] = useState<OptiscalerProfile[]>([]);
  const [profile, setProfile] = useState("default");
  const [onlyInstalled, setOnlyInstalled] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [results, setResults] = useState<Record<string, Result>>({});
  const [running, setRunning] = useState(false);
  const [confirmClose, setConfirmClose] = useState(false);
  const stop = useRef(false);

  // Same rule as the Graphics tab: an exe, Windows, and not a bundled game that opts out.
  const candidates = useMemo(
    () =>
      games
        .filter(
          (g) => !!g.executablePath && findBundledGame(g)?.optiscaler !== false
        )
        .sort((a, b) => a.title.localeCompare(b.title)),
    [games]
  );

  useEffect(() => {
    if (!visible) return;
    setResults({});
    // Each open starts from a clean slate, like the results.
    setSelected(new Set());
    void window.electron.getOptiscalerProfiles().then(setProfiles);
  }, [visible]);

  const toggle = (key: string) =>
    setSelected((current) => {
      const next = new Set(current);
      if (!next.delete(key)) next.add(key);
      return next;
    });

  const setResult = (key: string, result: Result) =>
    setResults((r) => ({ ...r, [key]: result }));

  const runOne = async (game: LibraryGame): Promise<Result> => {
    const tag = version || undefined;
    if (tool === "dxvk") {
      const s = await window.electron.getDxvkStatus(game.shop, game.objectId);
      if (onlyInstalled && !s.installed)
        return { state: "skipped", text: t("graphics_bulk_not_installed") };
      if (s.blockedBy)
        return {
          state: "skipped",
          text: t(`graphics_bulk_blocked_${s.blockedBy}`),
        };
      const { version: v } = await window.electron.installDxvk(
        game.shop,
        game.objectId,
        build,
        tag
      );
      return { state: "ok", text: t("graphics_bulk_ok", { version: v }) };
    }
    const s = await window.electron.getOptiscalerStatus(
      game.shop,
      game.objectId
    );
    if (onlyInstalled && !s.installed)
      return { state: "skipped", text: t("graphics_bulk_not_installed") };
    if (s.dxvkInstalled)
      return { state: "skipped", text: t("graphics_bulk_blocked_dxvk") };
    if (s.anticheat)
      return {
        state: "skipped",
        text: t("graphics_bulk_blocked_anticheat"),
      };
    // Keep the game's current proxy; new installs follow the Graphics tab default.
    const proxy = s.proxy ?? (s.api === "vulkan" ? "winmm.dll" : "dxgi.dll");
    if (!s.installed) {
      if (s.reshade === proxy)
        return { state: "skipped", text: t("graphics_bulk_blocked_reshade") };
      if (s.leftovers)
        return { state: "skipped", text: t("graphics_bulk_blocked_leftovers") };
      // The single-game flow asks before moving a foreign DLL to backup; bulk cannot ask, so it skips.
      const withProxy = await window.electron.getOptiscalerStatus(
        game.shop,
        game.objectId,
        proxy
      );
      if (withProxy.proxyConflict)
        return {
          state: "skipped",
          text: t("graphics_bulk_blocked_conflict", { proxy }),
        };
    }
    const { version: v } = await window.electron.installOptiscaler(
      game.shop,
      game.objectId,
      proxy,
      tag
    );
    if (profile !== "default")
      await window.electron.applyOptiscalerProfile(
        game.shop,
        game.objectId,
        profile
      );
    return { state: "ok", text: t("graphics_bulk_ok", { version: v }) };
  };

  const run = async () => {
    setRunning(true);
    stop.current = false;
    setResults({});
    // One game at a time: installs share one download and the game folders' disks.
    for (const game of candidates.filter((g) => selected.has(keyOf(g)))) {
      if (stop.current) break;
      const key = keyOf(game);
      setResult(key, { state: "running", text: t("graphics_bulk_running") });
      try {
        setResult(key, await runOne(game));
      } catch (err) {
        setResult(key, {
          state: "failed",
          text: t("graphics_bulk_failed", { message: errorText(err) }),
        });
      }
    }
    setRunning(false);
  };

  const done = Object.values(results);
  const summary = done.some((r) => r.state !== "running")
    ? t("graphics_bulk_summary", {
        ok: done.filter((r) => r.state === "ok").length,
        skipped: done.filter((r) => r.state === "skipped").length,
        failed: done.filter((r) => r.state === "failed").length,
      })
    : null;

  return (
    <Modal
      visible={visible}
      title={t("graphics_bulk_title")}
      description={t("graphics_bulk_description")}
      onClose={() => {
        // Closing mid-run would hide a game that is still installing; ask first.
        if (running) return setConfirmClose(true);
        onClose();
      }}
      large
    >
      <div className="graphics-bulk">
        {confirmClose && (
          <ConfirmBar
            tone="warn"
            confirmLabel={t("graphics_bulk_close_anyway")}
            onConfirm={() => {
              stop.current = true;
              setConfirmClose(false);
              onClose();
            }}
            onCancel={() => setConfirmClose(false)}
          >
            <p>{t("graphics_bulk_close_confirm")}</p>
          </ConfirmBar>
        )}
        <div className="graphics-bulk__options">
          <SelectField
            label={t("graphics_bulk_tool")}
            value={tool}
            disabled={running}
            options={[
              { key: "optiscaler", value: "optiscaler", label: "OptiScaler" },
              { key: "dxvk", value: "dxvk", label: "DXVK" },
            ]}
            onChange={(e) => {
              setTool(e.target.value as Tool);
              setVersion("");
              // Results belong to the tool that produced them.
              setResults({});
            }}
          />
          {tool === "dxvk" ? (
            <SelectField
              label={t("graphics_bulk_build")}
              value={build}
              disabled={running}
              options={[
                { key: "gplasync", value: "gplasync", label: "DXVK-gplasync" },
                { key: "dxvk", value: "dxvk", label: "DXVK" },
              ]}
              onChange={(e) => {
                setBuild(e.target.value as Build);
                setVersion("");
                setResults({});
              }}
            />
          ) : (
            <SelectField
              label={t("graphics_bulk_profile")}
              value={profile}
              disabled={running}
              options={profiles.map((p) => ({
                key: p.id,
                value: p.id,
                label: profileName(t, p),
              }))}
              onChange={(e) => setProfile(e.target.value)}
            />
          )}
          <VersionSelect
            tool={tool === "dxvk" ? build : "optiscaler"}
            value={version}
            disabled={running}
            onChange={setVersion}
          />
        </div>
        <CheckboxField
          label={t("graphics_bulk_only_installed")}
          checked={onlyInstalled}
          disabled={running}
          onChange={(e) => setOnlyInstalled(e.target.checked)}
        />

        <div className="graphics-bulk__actions">
          <Button
            theme="outline"
            disabled={running || !candidates.length}
            onClick={() =>
              setSelected(
                selected.size === candidates.length
                  ? new Set()
                  : new Set(candidates.map(keyOf))
              )
            }
          >
            {selected.size === candidates.length
              ? t("graphics_bulk_select_none")
              : t("graphics_bulk_select_all")}
          </Button>
          {running ? (
            <Button theme="danger" onClick={() => (stop.current = true)}>
              {t("graphics_bulk_stop")}
            </Button>
          ) : (
            <Button
              theme="primary"
              disabled={!selected.size}
              onClick={() => void run()}
            >
              {t("graphics_bulk_run", { count: selected.size })}
            </Button>
          )}
          {/* Always mounted so screen readers announce each new total, not each row. */}
          <span className="graphics-bulk__summary" aria-live="polite">
            {summary}
          </span>
        </div>

        {candidates.length ? (
          <ul className="graphics-bulk__list">
            {candidates.map((g) => {
              const key = keyOf(g);
              const r = results[key];
              return (
                <li key={key} className="graphics-bulk__row">
                  <CheckboxField
                    label={g.title}
                    checked={selected.has(key)}
                    disabled={running}
                    onChange={() => toggle(key)}
                  />
                  {r && (
                    <span
                      className={`graphics-bulk__result graphics-bulk__result--${r.state}`}
                    >
                      {r.text}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        ) : (
          <p>{t("graphics_bulk_none")}</p>
        )}
      </div>
    </Modal>
  );
}
