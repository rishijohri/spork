// Button primitives (UI_UX_DESIGN.md §3.6).
//
// One place for the control variants the design requires — primary / secondary /
// ghost / danger — each with the full hover/focus-visible/active/disabled/busy
// states the CSS defines, so a destructive action never looks like a read.

import type { ButtonHTMLAttributes, JSX, ReactNode } from "react";
import { Icon, type IconName } from "./icons";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

export interface ButtonProps
  extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: "sm" | "md";
  /** Optional leading icon. */
  icon?: IconName;
  /** Show an in-flight spinner and disable the button. */
  busy?: boolean;
  children?: ReactNode;
}

const VARIANT_CLASS: Record<ButtonVariant, string> = {
  primary: "btn--primary",
  secondary: "",
  ghost: "btn--ghost",
  danger: "btn--danger",
};

/** A labelled action button. */
export function Button({
  variant = "secondary",
  size = "md",
  icon,
  busy = false,
  disabled,
  className = "",
  children,
  ...rest
}: ButtonProps): JSX.Element {
  const classes = [
    "btn",
    VARIANT_CLASS[variant],
    size === "sm" ? "btn--sm" : "",
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button
      className={classes}
      disabled={disabled || busy}
      aria-disabled={disabled || busy ? true : undefined}
      data-busy={busy ? "true" : undefined}
      {...rest}
    >
      {icon && <Icon name={icon} size={14} />}
      {children}
    </button>
  );
}

export interface IconButtonProps
  extends ButtonHTMLAttributes<HTMLButtonElement> {
  icon: IconName;
  /** Accessible label (also the tooltip). */
  label: string;
  /** Active/toggled state. */
  active?: boolean;
  size?: number;
}

/** An icon-only button with an accessible label + tooltip. */
export function IconButton({
  icon,
  label,
  active = false,
  size = 16,
  className = "",
  ...rest
}: IconButtonProps): JSX.Element {
  return (
    <button
      className={["iconbtn", active ? "iconbtn--active" : "", className]
        .filter(Boolean)
        .join(" ")}
      aria-label={label}
      title={label}
      aria-pressed={active || undefined}
      {...rest}
    >
      <Icon name={icon} size={size} />
    </button>
  );
}
