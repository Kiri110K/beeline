export type ShowOrigin = "shortcut" | "test-hook";

export function shouldStartVisible(environment: Record<string, string | undefined>): boolean {
  return environment.VISUAL_FILES_START_VISIBLE === "1";
}
