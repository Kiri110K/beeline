import type { LifetimeSetting } from "../settings/schema";
import { isOnExcursion, type PinnedTab, type Tab, type TabId } from "./model";

// After this much continuous background time, every Pinned Excursion resets to
// its Anchor on the next show (§2, §4).
export const BACKGROUND_RESET_MS = 5 * 60 * 1000;

// The Temporary Tab lifetime in milliseconds, or `null` when it never expires (SPEC §4,
// §12). Settings drives this; it is the single reader consumers pass to `expiredTemporaryIds`.
export function lifetimeMs(setting: LifetimeSetting): number | null {
  return setting.kind === "never" ? null : setting.minutes * 60 * 1000;
}

// Pinned Tabs currently away from their Anchor — the Tabs a background reset
// returns home.
export function excursionsToReset(tabs: readonly Tab[]): PinnedTab[] {
  return tabs.filter(
    (tab): tab is PinnedTab => tab.kind === "pinned" && isOnExcursion(tab),
  );
}

// Temporary Tabs idle beyond their lifetime, excluding the active Tab which is
// never removed (it just became visible). Idle is measured from last activation. A
// `null` lifetime (Never, §4) expires nothing.
export function expiredTemporaryIds(
  tabs: readonly Tab[],
  activeId: TabId,
  nowMs: number,
  lifetimeMs: number | null,
): TabId[] {
  if (lifetimeMs === null) {
    return [];
  }
  const cutoff = nowMs - lifetimeMs;
  const expired: TabId[] = [];
  for (const tab of tabs) {
    if (
      tab.kind === "temporary" &&
      tab.id !== activeId &&
      tab.lastActivatedAtMs < cutoff
    ) {
      expired.push(tab.id);
    }
  }
  return expired;
}
