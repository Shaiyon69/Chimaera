import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@renderer/components";

import "./graphics-ui.scss";

type Tone = "warn" | "danger";

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
    // Button does not forward refs (React 18); Cancel is the first button.
    actions.current?.querySelector("button")?.focus();
    const timer = setTimeout(() => setArmed(true), ARM_DELAY_MS);
    return () => clearTimeout(timer);
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
