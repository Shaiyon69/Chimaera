import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { CheckIcon, SearchIcon, ZapIcon } from "@primer/octicons-react";
import cn from "classnames";

import {
  Button,
  CheckboxField,
  Modal,
  SelectField,
  TextField,
} from "@renderer/components";
import { ConfirmBar } from "@renderer/pages/game-details/modals/game-options-modal/graphics-ui";
import {
  profileName,
  type OptiscalerProfile,
} from "@renderer/pages/game-details/modals/game-options-modal/optiscaler-profiles";
import { VersionSelect } from "@renderer/pages/game-details/modals/game-options-modal/version-select";
import { findBundledGame } from "@shared";
import type { LibraryGame } from "@types";

import { resolveImageSource } from "./library-game-card";

import "./graphics-bulk-modal.scss";

type Tool = "optiscaler" | "dxvk";
type Build = "gplasync" | "dxvk";
type Filter = "all" | "upscaler" | "installed";
type Result = { state: "running" | "ok" | "skipped" | "failed"; text: string };
type Overview = Awaited<
  ReturnType<typeof window.electron.getGraphicsOverview>
>[string];

const keyOf = (g: LibraryGame) => `${g.shop}:${g.objectId}`;
const errorText = (err: unknown) =>
  err instanceof Error ? err.message : String(err);

// Chips name the family, like the game's own settings menu does.
const CHIPS = [
  { label: "DLSS", tone: "nvidia", kinds: ["dlss", "dlssg", "dlssd"] },
  { label: "FSR", tone: "amd", kinds: ["fsr", "fsrfg"] },
  { label: "XeSS", tone: "intel", kinds: ["xess", "xefg"] },
] as const;

/** Cover with the same fallback order as the library grid. */
function Cover({ game }: Readonly<{ game: LibraryGame }>) {
  const sources = [
    game.customCoverImageUrl,
    game.coverImageUrl,
    game.libraryImageUrl,
    game.iconUrl,
  ]
    .map(resolveImageSource)
    .filter((src, i, all) => src && all.indexOf(src) === i);
  const [index, setIndex] = useState(0);

  if (index >= sources.length)
    return <span className="graphics-bulk__cover-fallback">{game.title}</span>;
  return (
    <img
      src={sources[index]}
      alt=""
      loading="lazy"
      onError={() => setIndex((i) => i + 1)}
    />
  );
}

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
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [overview, setOverview] = useState<Record<string, Overview>>({});
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

  const loadOverview = () =>
    window.electron
      .getGraphicsOverview(
        candidates.map((g) => ({ shop: g.shop, objectId: g.objectId }))
      )
      .then(setOverview)
      .catch(() => setOverview({}));

  useEffect(() => {
    if (!visible) return;
    setResults({});
    // Each open starts from a clean slate, like the results.
    setSelected(new Set());
    setQuery("");
    void window.electron
      .getOptiscalerProfiles()
      .then(setProfiles)
      .catch(() => setProfiles([]));
    void loadOverview();
    // loadOverview only reads candidates, which is already a dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, candidates]);

  const installedVersion = (key: string) =>
    tool === "dxvk" ? overview[key]?.dxvk : overview[key]?.optiscaler;

  const shown = candidates.filter((g) => {
    const key = keyOf(g);
    if (query && !g.title.toLowerCase().includes(query.toLowerCase()))
      return false;
    if (filter === "upscaler") return !!overview[key]?.upscalers.length;
    if (filter === "installed") return !!installedVersion(key);
    return true;
  });
  const allShownSelected =
    shown.length > 0 && shown.every((g) => selected.has(keyOf(g)));

  const toggle = (key: string) =>
    setSelected((current) => {
      const next = new Set(current);
      if (!next.delete(key)) next.add(key);
      return next;
    });

  const toggleShown = () =>
    setSelected((current) => {
      const next = new Set(current);
      for (const g of shown)
        if (allShownSelected) next.delete(keyOf(g));
        else next.add(keyOf(g));
      return next;
    });

  const setResult = (key: string, result: Result) =>
    setResults((r) => ({ ...r, [key]: result }));

  const runOne = async (game: LibraryGame): Promise<Result> => {
    const tag = version || undefined;
    const ok = (v: string): Result => ({
      state: "ok",
      text: t("graphics_bulk_ok", { version: v }),
    });
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
      return ok(v);
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
      return { state: "skipped", text: t("graphics_bulk_blocked_anticheat") };
    // Keep the game's current proxy; new installs follow the Graphics tab default.
    const proxy = s.proxy ?? (s.api === "vulkan" ? "winmm.dll" : "dxgi.dll");
    if (!s.installed) {
      if (s.reshade === proxy)
        return { state: "skipped", text: t("graphics_bulk_blocked_reshade") };
      if (s.leftovers)
        return {
          state: "skipped",
          text: t("graphics_bulk_blocked_leftovers"),
        };
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
    return ok(v);
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
    void loadOverview();
  };

  const switchTool = (next: Tool) => {
    if (running || next === tool) return;
    setTool(next);
    setVersion("");
    // Results belong to the tool that produced them.
    setResults({});
  };

  const done = Object.values(results);
  const summary = done.some((r) => r.state !== "running")
    ? t("graphics_bulk_summary", {
        ok: done.filter((r) => r.state === "ok").length,
        skipped: done.filter((r) => r.state === "skipped").length,
        failed: done.filter((r) => r.state === "failed").length,
      })
    : t("graphics_bulk_selected", { count: selected.size });

  const segmented = <T extends string>(
    label: string,
    value: T,
    options: { value: T; label: string }[],
    onPick: (v: T) => void
  ) => (
    <div
      className="graphics-bulk__segmented"
      role="radiogroup"
      aria-label={label}
    >
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={value === o.value}
          className={cn("graphics-bulk__segment", {
            "graphics-bulk__segment--active": value === o.value,
          })}
          onClick={() => onPick(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );

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
      className="graphics-bulk-modal"
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

        <div className="graphics-bulk__config">
          {segmented(
            t("graphics_bulk_tool"),
            tool,
            [
              { value: "optiscaler", label: "OptiScaler" },
              { value: "dxvk", label: "DXVK" },
            ],
            switchTool
          )}
          <div className="graphics-bulk__fields">
            {tool === "dxvk" ? (
              <SelectField
                label={t("graphics_bulk_build")}
                value={build}
                disabled={running}
                options={[
                  {
                    key: "gplasync",
                    value: "gplasync",
                    label: "DXVK-gplasync",
                  },
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
                options={
                  profiles.length
                    ? profiles.map((p) => ({
                        key: p.id,
                        value: p.id,
                        label: profileName(t, p),
                      }))
                    : [
                        {
                          key: "default",
                          value: "default",
                          label: t("optiscaler_profile_default", {
                            ns: "game_details",
                          }),
                        },
                      ]
                }
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
        </div>

        <div className="graphics-bulk__toolbar">
          <div className="graphics-bulk__search">
            <TextField
              theme="dark"
              value={query}
              placeholder={t("graphics_bulk_search")}
              aria-label={t("graphics_bulk_search")}
              onChange={(e) => setQuery(e.target.value)}
              rightContent={<SearchIcon size={16} />}
            />
          </div>
          {segmented(
            t("graphics_bulk_filter_label"),
            filter,
            [
              { value: "all", label: t("graphics_bulk_filter_all") },
              { value: "upscaler", label: t("graphics_bulk_filter_upscaler") },
              {
                value: "installed",
                label: t("graphics_bulk_filter_installed"),
              },
            ],
            setFilter
          )}
          <span className="graphics-bulk__summary" aria-live="polite">
            {summary}
          </span>
          <div className="graphics-bulk__actions">
            <Button
              theme="outline"
              aria-disabled={running || !shown.length}
              onClick={() => !running && shown.length && toggleShown()}
            >
              {allShownSelected
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
                aria-disabled={!selected.size}
                onClick={() => selected.size && void run()}
              >
                <ZapIcon size={16} />
                {t("graphics_bulk_run", { count: selected.size })}
              </Button>
            )}
          </div>
        </div>

        {!candidates.length && (
          <p className="graphics-bulk__empty">{t("graphics_bulk_none")}</p>
        )}
        {!!candidates.length && !shown.length && (
          <p className="graphics-bulk__empty">{t("graphics_bulk_no_match")}</p>
        )}
        <ul className="graphics-bulk__grid">
          {shown.map((g) => {
            const key = keyOf(g);
            const r = results[key];
            const isSelected = selected.has(key);
            const info = overview[key];
            const ribbon = info?.optiscaler
              ? `OptiScaler ${info.optiscaler}`
              : info?.dxvk
                ? `DXVK ${info.dxvk}`
                : null;
            return (
              <li key={key}>
                <button
                  type="button"
                  aria-pressed={isSelected}
                  aria-disabled={running}
                  className={cn("graphics-bulk__card", {
                    "graphics-bulk__card--selected": isSelected,
                  })}
                  onClick={() => !running && toggle(key)}
                >
                  <span className="graphics-bulk__cover">
                    <Cover game={g} />
                    <span className="graphics-bulk__check" aria-hidden="true">
                      {isSelected && <CheckIcon size={14} />}
                    </span>
                    {ribbon && (
                      <span className="graphics-bulk__ribbon">
                        <ZapIcon size={12} />
                        {ribbon}
                      </span>
                    )}
                    {r?.state === "running" && (
                      <span className="graphics-bulk__scrim">
                        <span className="graphics-bulk__spinner" />
                      </span>
                    )}
                  </span>
                  <span className="graphics-bulk__title">{g.title}</span>
                  {!!info?.upscalers.length && (
                    <span className="graphics-bulk__chips">
                      {CHIPS.filter((c) =>
                        c.kinds.some((k) => info.upscalers.includes(k))
                      ).map((c) => (
                        <span
                          key={c.label}
                          className={`graphics-bulk__chip graphics-bulk__chip--${c.tone}`}
                        >
                          {c.label}
                        </span>
                      ))}
                    </span>
                  )}
                  {r && r.state !== "running" && (
                    <span
                      className={`graphics-bulk__result graphics-bulk__result--${r.state}`}
                    >
                      {r.text}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </Modal>
  );
}
