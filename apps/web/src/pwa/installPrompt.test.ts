// Runs in the default `node` env: the module guards all window/navigator
// access, and Node's global Event is enough to exercise capture/prompt.
import { describe, expect, it } from "vitest";
import {
  captureInstallPrompt,
  isIos,
  isStandalone,
  promptInstall,
} from "./installPrompt";

function fakePromptEvent(outcome: "accepted" | "dismissed") {
  const e = new Event("beforeinstallprompt", { cancelable: true });
  let prompted = false;
  Object.assign(e, {
    prompt: () => {
      prompted = true;
      return Promise.resolve();
    },
    userChoice: Promise.resolve({ outcome, platform: "web" }),
  });
  return {
    event: e,
    wasPrompted: () => prompted,
  };
}

describe("installPrompt", () => {
  it("returns unavailable when nothing was captured", async () => {
    expect(await promptInstall()).toBe("unavailable");
  });

  it("captures the event (preventDefault) and prompts once", async () => {
    const { event, wasPrompted } = fakePromptEvent("accepted");
    captureInstallPrompt(event);
    expect(event.defaultPrevented).toBe(true);

    expect(await promptInstall()).toBe("accepted");
    expect(wasPrompted()).toBe(true);

    // Single-use: the deferred event is consumed by the prompt.
    expect(await promptInstall()).toBe("unavailable");
  });

  it("propagates a dismissal", async () => {
    const { event } = fakePromptEvent("dismissed");
    captureInstallPrompt(event);
    expect(await promptInstall()).toBe("dismissed");
  });

  it("environment probes do not throw without a DOM", () => {
    // node env: no window, generic (or absent) navigator.
    expect(isStandalone()).toBe(false);
    expect(isIos()).toBe(false);
  });
});
