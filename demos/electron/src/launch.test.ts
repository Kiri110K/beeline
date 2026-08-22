import { describe, expect, test } from "bun:test";
import { shouldStartVisible } from "../electron/launch";

describe("test-only visible launch hook", () => {
  test("is disabled by default", () => {
    expect(shouldStartVisible({})).toBe(false);
  });

  test("requires the exact opt-in value", () => {
    expect(shouldStartVisible({ VISUAL_FILES_START_VISIBLE: "1" })).toBe(true);
    expect(shouldStartVisible({ VISUAL_FILES_START_VISIBLE: "true" })).toBe(false);
    expect(shouldStartVisible({ VISUAL_FILES_START_VISIBLE: "0" })).toBe(false);
  });
});
