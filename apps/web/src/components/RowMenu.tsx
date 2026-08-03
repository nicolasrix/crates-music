// Shell for the "⋯" row menus (track / album / artist). Owns everything
// that isn't the action list itself: the trigger button, panel placement
// (see rowMenuCoords), portalling, and dismissal via click-outside or
// Escape. Callers supply entries through a render prop that receives a
// `close` callback.
//
// The panel body mounts only while open, so state a caller keeps inside
// it — the track menu's playlist submenu view, say — resets on every
// open without this shell knowing such state exists.
//
// Portal into <body>: any ancestor with `backdrop-filter`, `transform`,
// `filter`, `perspective`, `will-change`, or `contain` becomes the
// containing block for `position: fixed` descendants, overriding the
// viewport. The PlayerBar uses `backdrop-filter: blur(20px)` and would
// otherwise re-anchor the panel off-screen. Portalling sidesteps the
// trap entirely and flattens z-index across all consumers.

import { MoreHorizontal } from "lucide-react";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { menuCoords, type MenuCoords } from "./rowMenuCoords";

/** Caller-supplied entry rendered at the top of a menu's root view.
 *  Used by the Queue page for reorder actions — on phones the chevron
 *  buttons are hidden (≤640px), so the menu is the touch-reachable path. */
export interface RowMenuExtraItem {
  key: string;
  label: string;
  icon?: ReactNode;
  onClick: () => void;
  disabled?: boolean;
}

export function RowMenu({
  label,
  children,
}: {
  /** Accessible name for the trigger, e.g. "album options". */
  label: string;
  children: (close: () => void) => ReactNode;
}) {
  // Coords double as the open flag: a panel with no placement can't be
  // rendered anyway, so a separate `open` boolean could only ever
  // disagree with reality.
  const [coords, setCoords] = useState<MenuCoords | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const open = coords !== null;
  const close = useCallback(() => setCoords(null), []);

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className="row-menu-trigger"
        onClick={(e) => {
          // Rows are click-to-play and cards are click-to-navigate; the
          // menu must not trigger either.
          e.stopPropagation();
          if (open) {
            close();
            return;
          }
          const el = triggerRef.current;
          if (!el) return;
          setCoords(
            menuCoords(el.getBoundingClientRect(), {
              width: window.innerWidth,
              height: window.innerHeight,
            }),
          );
        }}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
      >
        <MoreHorizontal size={16} strokeWidth={1.5} />
      </button>
      {coords && (
        <RowMenuPanel coords={coords} onClose={close}>
          {children(close)}
        </RowMenuPanel>
      )}
    </>
  );
}

function RowMenuPanel({
  coords,
  onClose,
  children,
}: {
  coords: MenuCoords;
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    function onDown(e: MouseEvent) {
      if (!ref.current) return;
      if (!ref.current.contains(e.target as Node)) onClose();
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  return createPortal(
    <div
      ref={ref}
      className="row-menu"
      role="menu"
      style={{
        left: coords.left,
        ...("top" in coords ? { top: coords.top } : { bottom: coords.bottom }),
      }}
    >
      {children}
    </div>,
    document.body,
  );
}

/** One entry. `icon` sits before the label so every menu lines up on the
 *  same 14px glyph column; `trailing` is for the submenu chevron, which
 *  the label's `flex: 1` pushes to the right edge. Labels ellipsise
 *  rather than wrap — a user-named playlist can be arbitrarily long and
 *  the panel is a fixed 220px. */
export function RowMenuItem({
  icon,
  onClick,
  disabled,
  trailing,
  className,
  children,
}: {
  icon?: ReactNode;
  onClick: () => void;
  disabled?: boolean | undefined;
  trailing?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      className={className ? `row-menu-item ${className}` : "row-menu-item"}
      onClick={onClick}
      disabled={disabled}
    >
      {icon}
      <span className="row-menu-label">{children}</span>
      {trailing}
    </button>
  );
}

export function RowMenuSep() {
  return <div className="row-menu-sep" />;
}

/** Renders the caller-supplied entries plus their trailing separator, or
 *  nothing at all when there are none. Shared so each menu doesn't
 *  re-derive "separator only if the list is non-empty". */
export function RowMenuExtras({
  items,
  onClose,
}: {
  items: RowMenuExtraItem[] | undefined;
  onClose: () => void;
}) {
  if (!items || items.length === 0) return null;
  return (
    <>
      {items.map((it) => (
        <RowMenuItem
          key={it.key}
          icon={it.icon}
          disabled={it.disabled}
          onClick={() => {
            it.onClick();
            onClose();
          }}
        >
          {it.label}
        </RowMenuItem>
      ))}
      <RowMenuSep />
    </>
  );
}
