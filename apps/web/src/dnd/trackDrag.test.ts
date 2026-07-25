// Unit tests for the desktop track drag-and-drop helpers. Runs in vitest's
// `node` env (no DOM), so the DataTransfer and the document used by the
// drag-image builder are minimal hand-rolled fakes — same no-new-deps shim
// style used elsewhere in the suite.

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  TRACK_DND_MIME,
  beginTrackDrag,
  getTrackDragData,
  isTrackDrag,
  setTrackDragData,
} from "./trackDrag";

/** Minimal stand-in for the bits of DataTransfer these helpers touch. */
function fakeDataTransfer() {
  const store = new Map<string, string>();
  const setDragImage = vi.fn();
  return {
    types: [] as string[],
    effectAllowed: "" as string,
    setData(type: string, val: string) {
      store.set(type, val);
      if (!this.types.includes(type)) this.types.push(type);
    },
    getData(type: string) {
      return store.get(type) ?? "";
    },
    setDragImage,
  };
}

/** A tiny fake DOM sufficient for setTrackDragImage: createElement returns a
 *  node with the properties the builder writes, and body records appends. */
function installFakeDom() {
  const appended: FakeEl[] = [];
  class FakeEl {
    className = "";
    textContent = "";
    children: FakeEl[] = [];
    attrs: Record<string, string> = {};
    style: Record<string, string> = {};
    removed = false;
    setAttribute(k: string, v: string) {
      this.attrs[k] = v;
    }
    append(...kids: FakeEl[]) {
      this.children.push(...kids);
    }
    remove() {
      this.removed = true;
    }
  }
  const doc = {
    createElement: () => new FakeEl(),
    body: {
      appendChild: (el: FakeEl) => {
        appended.push(el);
        return el;
      },
    },
  };
  (globalThis as { document?: unknown }).document = doc;
  return { appended, FakeEl };
}

afterEach(() => {
  delete (globalThis as { document?: unknown }).document;
  vi.useRealTimers();
});

describe("track drag payload", () => {
  it("round-trips the track id through the custom MIME type", () => {
    const dt = fakeDataTransfer();
    setTrackDragData(dt as unknown as DataTransfer, "trk-42");

    expect(dt.getData(TRACK_DND_MIME)).toBe("trk-42");
    expect(dt.effectAllowed).toBe("copy");
    expect(isTrackDrag(dt as unknown as DataTransfer)).toBe(true);
    expect(getTrackDragData(dt as unknown as DataTransfer)).toBe("trk-42");
  });

  it("reports no track drag and null id for an empty transfer", () => {
    const dt = fakeDataTransfer();
    expect(isTrackDrag(dt as unknown as DataTransfer)).toBe(false);
    expect(getTrackDragData(dt as unknown as DataTransfer)).toBeNull();
  });
});

describe("beginTrackDrag drag image", () => {
  it("stamps the payload and registers a titled pill as the drag image", () => {
    vi.useFakeTimers();
    const { appended } = installFakeDom();
    const dt = fakeDataTransfer();

    beginTrackDrag(dt as unknown as DataTransfer, "trk-7", "Blue in Green");

    // Payload still set.
    expect(getTrackDragData(dt as unknown as DataTransfer)).toBe("trk-7");
    // A chip was appended and registered as the drag image.
    expect(appended).toHaveLength(1);
    const chip = appended[0]!;
    expect(chip.className).toBe("track-drag-chip");
    expect(dt.setDragImage).toHaveBeenCalledWith(chip, 12, 14);
    // The label rides on the chip.
    const label = chip.children.find((c) => c.className === "track-drag-chip__label");
    expect(label?.textContent).toBe("Blue in Green");

    // The off-screen node is cleaned up on the next tick.
    expect(chip.removed).toBe(false);
    vi.runAllTimers();
    expect(chip.removed).toBe(true);
  });

  it("is a no-op on the drag image when there is no document (SSR/node)", () => {
    // No installFakeDom() → globalThis.document is undefined.
    const dt = fakeDataTransfer();
    expect(() =>
      beginTrackDrag(dt as unknown as DataTransfer, "trk-9", "Naima"),
    ).not.toThrow();
    // Data path still runs even without a DOM.
    expect(getTrackDragData(dt as unknown as DataTransfer)).toBe("trk-9");
    expect(dt.setDragImage).not.toHaveBeenCalled();
  });
});
