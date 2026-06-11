// Minimal toast primitive. `useToast()` returns a stable function:
//
//   toast("added to queue", { variant: "success" })
//
// Toasts stack bottom-center above the player bar, auto-dismiss (errors
// linger longer), and dismiss on tap. This replaces `window.alert` —
// which iOS standalone PWAs may suppress entirely — and gives success
// paths (queue adds, downloads) a feedback channel they never had.
//
// Repeated identical messages don't stack: re-toasting one restarts its
// timer instead. Rapid-fire actions ("add to queue" ×5) read as one
// persistent toast, not a tower.

import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

export type ToastVariant = "info" | "success" | "error";

export interface ToastOptions {
  variant?: ToastVariant;
  /** Auto-dismiss delay in ms. Defaults: 3500, errors 6000. */
  duration?: number;
}

export type ToastFn = (message: string, opts?: ToastOptions) => void;

interface ToastItem {
  id: number;
  message: string;
  variant: ToastVariant;
}

const MAX_VISIBLE = 4;

const ToastContext = createContext<ToastFn | null>(null);

export function useToast(): ToastFn {
  const fn = useContext(ToastContext);
  if (!fn) throw new Error("useToast must be used inside <ToastProvider>");
  return fn;
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<readonly ToastItem[]>([]);
  // Mirror of `items` for reads outside render. All list logic computes
  // against the ref and commits both — keeping timer side effects out of
  // setState updaters, which StrictMode double-invokes.
  const itemsRef = useRef<readonly ToastItem[]>([]);
  const timersRef = useRef<Map<number, number>>(new Map());
  const nextIdRef = useRef(1);

  const commit = useCallback((next: readonly ToastItem[]) => {
    itemsRef.current = next;
    setItems(next);
  }, []);

  const dismiss = useCallback(
    (id: number) => {
      const timer = timersRef.current.get(id);
      if (timer !== undefined) {
        clearTimeout(timer);
        timersRef.current.delete(id);
      }
      commit(itemsRef.current.filter((it) => it.id !== id));
    },
    [commit],
  );

  const toast = useCallback<ToastFn>(
    (message, opts) => {
      const variant = opts?.variant ?? "info";
      const duration = opts?.duration ?? (variant === "error" ? 6000 : 3500);

      const dup = itemsRef.current.find(
        (it) => it.message === message && it.variant === variant,
      );
      if (dup) {
        const timer = timersRef.current.get(dup.id);
        if (timer !== undefined) clearTimeout(timer);
        timersRef.current.set(
          dup.id,
          window.setTimeout(() => dismiss(dup.id), duration),
        );
        return;
      }

      const id = nextIdRef.current++;
      timersRef.current.set(
        id,
        window.setTimeout(() => dismiss(id), duration),
      );
      let next: readonly ToastItem[] = [
        ...itemsRef.current,
        { id, message, variant },
      ];
      // Over the cap: drop the oldest (and its timer) rather than queue.
      while (next.length > MAX_VISIBLE) {
        const drop = next[0]!;
        const timer = timersRef.current.get(drop.id);
        if (timer !== undefined) clearTimeout(timer);
        timersRef.current.delete(drop.id);
        next = next.slice(1);
      }
      commit(next);
    },
    [commit, dismiss],
  );

  return (
    <ToastContext.Provider value={toast}>
      {children}
      {createPortal(
        // polite, not assertive: a queue-add confirmation shouldn't
        // interrupt a screen reader mid-sentence.
        <div className="toast-viewport" role="status" aria-live="polite">
          {items.map((it) => (
            <button
              key={it.id}
              type="button"
              className={`toast is-${it.variant}`}
              onClick={() => dismiss(it.id)}
            >
              {it.message}
            </button>
          ))}
        </div>,
        document.body,
      )}
    </ToastContext.Provider>
  );
}
