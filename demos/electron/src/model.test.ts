import { describe, expect, test } from "bun:test";
import { benchmarkItems, clampSelection, formatSize } from "./model";

describe("benchmark data", () => {
  test("generates deterministic unique rows", () => {
    const first = benchmarkItems();
    const second = benchmarkItems();
    expect(first).toHaveLength(10_000);
    expect(first).toEqual(second);
    expect(new Set(first.map((item) => item.id)).size).toBe(10_000);
  });
});

test("selection clamps around an empty or bounded list", () => {
  expect(clampSelection(3, 0)).toBe(-1);
  expect(clampSelection(-2, 5)).toBe(0);
  expect(clampSelection(9, 5)).toBe(4);
});

test("file sizes use compact units", () => {
  expect(formatSize(null)).toBe("");
  expect(formatSize(100)).toBe("100 B");
  expect(formatSize(1536)).toBe("1.5 KB");
});
