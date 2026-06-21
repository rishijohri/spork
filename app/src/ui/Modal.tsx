// Modal primitive (UI_UX_DESIGN.md §5.10).
//
// A single elevation-3 dialog shell: overlay + titled card + a right-aligned
// action row. Closes on Escape and on overlay click; traps initial focus on the
// dialog. The action row is provided by the caller (secondary + primary), so a
// destructive primary can use the danger variant.

import { useEffect, useRef, type JSX, type ReactNode } from "react";
import { Icon, type IconName } from "./icons";
import { IconButton } from "./Button";

export interface ModalProps {
  title: string;
  icon?: IconName;
  onClose: () => void;
  children: ReactNode;
  /** The action row (typically Cancel + a primary). */
  footer?: ReactNode;
  /** Accessible label fallback if title is decorative. */
  ariaLabel?: string;
}

/** A focus-trapping, Escape-dismissable modal dialog. */
export function Modal({
  title,
  icon,
  onClose,
  children,
  footer,
  ariaLabel,
}: ModalProps): JSX.Element {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    ref.current?.focus();
    function onKey(e: KeyboardEvent): void {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="spork-overlay"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="spork-modal"
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel ?? title}
        tabIndex={-1}
        ref={ref}
      >
        <header className="spork-modal-head">
          {icon && <Icon name={icon} size={16} />}
          <h2 className="spork-modal-title">{title}</h2>
          <div style={{ flex: 1 }} />
          <IconButton icon="x" label="Close" onClick={onClose} />
        </header>
        <div className="spork-modal-body">{children}</div>
        {footer && <div className="spork-modal-actions">{footer}</div>}
      </div>
    </div>
  );
}
