export type ItemKind = "directory" | "file" | "other";

export interface FileItem {
  id: string;
  path: string | null;
  name: string;
  kind: ItemKind;
  modifiedMs: number | null;
  size: number | null;
  synthetic?: boolean;
}

export interface LocationResult {
  location: string;
  selectPath: string | null;
  items: FileItem[];
  durationMs: number;
}

export interface RecentResult {
  items: FileItem[];
  durationMs: number;
  warning?: string;
}
