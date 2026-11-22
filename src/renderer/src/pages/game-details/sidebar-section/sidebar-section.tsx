import { ChevronDownIcon, LinkExternalIcon } from "@primer/octicons-react";
import { useState } from "react";
import "./sidebar-section.scss";

export interface SidebarSectionProps {
  title: string;
  subtitle?: string;
  subtitleHref?: string;
  children: React.ReactNode;
}

export function SidebarSection({
  title,
  subtitle,
  subtitleHref,
  children,
}: SidebarSectionProps) {
  const [isOpen, setIsOpen] = useState(true);

  return (
    <div className="sidebar-section">
      <div className="sidebar-section__header">
        <button
          onClick={() => setIsOpen(!isOpen)}
          className="sidebar-section__toggle"
          aria-expanded={isOpen}
          aria-label={title}
        />

        <span className="sidebar-section__button">
          <ChevronDownIcon
            className={`sidebar-section__chevron ${
              isOpen ? "sidebar-section__chevron--open" : ""
            }`}
          />
          <span>{title}</span>
        </span>

        {subtitle && subtitleHref && (
          <a
            href={subtitleHref}
            className="sidebar-section__subtitle"
            target="_blank"
            rel="noreferrer"
          >
            {subtitle}
            <LinkExternalIcon size={12} />
          </a>
        )}
      </div>

      {/* A grid row sized 1fr/0fr: it follows content that grows after load (icons, lazy
          sections), where a measured max-height clipped it. */}
      <div
        className={`sidebar-section__content${
          isOpen ? " sidebar-section__content--open" : ""
        }`}
      >
        <div className="sidebar-section__inner">{children}</div>
      </div>
    </div>
  );
}
