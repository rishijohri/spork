// A thin drag handle for resizing a panel (UI_UX_DESIGN.md §4 — resizable panes).
//
// Pointer-driven: on press it tracks the cursor and reports a clamped new size to
// `onChange`. `axis="x"` resizes a width, `axis="y"` a height; `sign` flips the
// direction (e.g. a handle on a panel's LEFT edge grows the panel as the cursor
// moves left → sign = -1). Keyboard-accessible (arrow keys nudge by 16px).

import { useCallback, type JSX } from "react";

export interface ResizeHandleProps {
  /** Which dimension this handle resizes. */
  axis: "x" | "y";
  /** +1 if moving the cursor in the positive axis direction grows the value, else -1. */
  sign: 1 | -1;
  /** The current size (px). */
  value: number;
  /** Clamp bounds (px). */
  min: number;
  max: number;
  /** Report a new clamped size. */
  onChange: (px: number) => void;
  /** Accessible label (e.g. "Resize navigator"). */
  label: string;
}

/** A draggable divider that resizes an adjacent panel. */
export function ResizeHandle({
  axis,
  sign,
  value,
  min,
  max,
  onChange,
  label,
}: ResizeHandleProps): JSX.Element {
  const clamp = useCallback((v: number) => Math.max(min, Math.min(max, v)), [min, max]);

  function onPointerDown(e: React.PointerEvent<HTMLDivElement>): void {
    e.preventDefault();
    const startPos = axis === "x" ? e.clientX : e.clientY;
    const startVal = value;
    (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
    document.body.classList.add("spork-resizing");

    function move(ev: PointerEvent): void {
      const cur = axis === "x" ? ev.clientX : ev.clientY;
      onChange(clamp(startVal + sign * (cur - startPos)));
    }
    function up(): void {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      document.body.classList.remove("spork-resizing");
    }
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>): void {
    const grow = axis === "x" ? "ArrowRight" : "ArrowDown";
    const shrink = axis === "x" ? "ArrowLeft" : "ArrowUp";
    if (e.key === grow) onChange(clamp(value + sign * 16));
    else if (e.key === shrink) onChange(clamp(value - sign * 16));
    else return;
    e.preventDefault();
  }

  return (
    <div
      className={`spork-resize spork-resize--${axis}`}
      role="separator"
      aria-orientation={axis === "x" ? "vertical" : "horizontal"}
      aria-label={label}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
    />
  );
}
