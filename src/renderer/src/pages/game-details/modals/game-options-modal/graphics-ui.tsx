import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@renderer/components";

import "./graphics-ui.scss";

type Tone = "warn" | "danger";

export type Upscaler = Awaited<
  ReturnType<typeof window.electron.getGameUpscalers>
>[number];

/** One shared busy state for the tab: OptiScaler and DXVK both write dxgi.dll. */
export type GraphicsBusy = {
  owner: "optiscaler" | "dxvk";
  label: string;
} | null;

/**
 * Which tool fits this game best. DX9/DX10 is DXVK territory (OptiScaler does not
 * cover it); a DX11 game with no upscaler DLLs gives OptiScaler nothing to replace.
 * Null while the upscaler scan is still running.
 */
export function suggestedTool(
  api: string,
  upscalers: Upscaler[] | null
): "optiscaler" | "dxvk" | null {
  if (api === "dx9" || api === "dx10") return "dxvk";
  if (api === "dx11") {
    if (!upscalers) return null;
    return upscalers.length ? "optiscaler" : "dxvk";
  }
  return api === "dx12" || api === "vulkan" ? "optiscaler" : null;
}

/** A tinted note; danger notes interrupt screen readers, warnings wait their turn. */
export function Notice({
  tone,
  children,
}: Readonly<{ tone: Tone; children: React.ReactNode }>) {
  return (
    <div
      className={`graphics-notice graphics-notice--${tone}`}
      role={tone === "danger" ? "alert" : "status"}
    >
      {children}
    </div>
  );
}

// Long enough to swallow the second click of a double-click on the button that armed this.
const ARM_DELAY_MS = 600;

/**
 * Confirmation for a risky action. The confirm button is a separate control that ignores
 * clicks for a moment, and focus starts on Cancel, so a double-click or a stray Enter
 * on the original button cannot confirm by accident.
 */
export function ConfirmBar({
  tone = "danger",
  children,
  confirmLabel,
  onConfirm,
  onCancel,
}: Readonly<{
  tone?: Tone;
  children: React.ReactNode;
  confirmLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
}>) {
  const { t } = useTranslation("game_details");
  const [armed, setArmed] = useState(false);
  const actions = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // Remember who opened the bar, so focus goes back there when it closes.
    const opener =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    // Button does not forward refs (React 18); Cancel is the first button.
    actions.current?.querySelector("button")?.focus();
    const timer = setTimeout(() => setArmed(true), ARM_DELAY_MS);
    return () => {
      clearTimeout(timer);
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  return (
    <Notice tone={tone}>
      <div className="graphics-notice__body">{children}</div>
      <div ref={actions} className="graphics-notice__actions">
        <Button theme="outline" onClick={onCancel}>
          {t("graphics_cancel")}
        </Button>
        <Button
          theme="danger"
          aria-disabled={!armed}
          onClick={() => armed && onConfirm()}
        >
          {confirmLabel}
        </Button>
      </div>
    </Notice>
  );
}

/** Collapsed group built on <details>, so it opens with the keyboard for free. */
export function Disclosure({
  summary,
  defaultOpen = false,
  children,
}: Readonly<{
  summary: string;
  defaultOpen?: boolean;
  children: React.ReactNode;
}>) {
  return (
    <details className="graphics-disclosure" open={defaultOpen}>
      <summary className="graphics-disclosure__summary">{summary}</summary>
      <div className="graphics-disclosure__body">{children}</div>
    </details>
  );
}

/** Live line for progress and results; screen readers announce each change. */
export function StatusLine({ text }: Readonly<{ text: string | null }>) {
  return (
    <p className="graphics-status" aria-live="polite">
      {text}
    </p>
  );
}
