import type { FileItem, LocationResult, RecentResult } from "../electron/types";

export {};

declare global {
  interface Window {
    visualFiles: {
      loadPath(input: string): Promise<LocationResult>;
      loadRecents(): Promise<RecentResult>;
      openPath(input: string): Promise<string>;
      copyPath(input: string): Promise<void>;
      quickLook(input: string | null): Promise<{ open: boolean; workaround: string }>;
      hideWindow(): Promise<void>;
      frontendReady(): void;
      visibleAndFocused(focused: boolean): void;
      benchmarkRendered(count: number): void;
      onFocusPath(callback: () => void): () => void;
    };
  }
}

export type { FileItem };
