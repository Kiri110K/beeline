import type { FileItem } from "../electron/types";

export function benchmarkItems(count = 10_000): FileItem[] {
  return Array.from({ length: count }, (_, index) => {
    const number = String(index + 1).padStart(5, "0");
    const directory = index % 17 === 0;
    return {
      id: `benchmark-${number}`,
      path: null,
      name: directory ? `Folder ${number}` : `Document ${number}.md`,
      kind: directory ? "directory" : "file",
      modifiedMs: Date.UTC(2026, 0, 1) - index * 60_000,
      size: directory ? null : 1024 + ((index * 7919) % 5_000_000),
      synthetic: true,
    };
  });
}

export function clampSelection(index: number, itemCount: number): number {
  if (itemCount <= 0) return -1;
  return Math.max(0, Math.min(index, itemCount - 1));
}

export function formatSize(bytes: number | null): string {
  if (bytes === null) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}
