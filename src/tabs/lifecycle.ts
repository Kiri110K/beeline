import { type PinnedTab, type Tab, type TabId } from "./model";

// After this much continuous background time, every Pinned Excursion resets to
// its Anchor on the next show (§2, §4).
export const BACKGROUND_RESET_MS = 5 * 60 * 1000;

// A Temporary Tab idle past its lifetime is removed in the background or on the
// next show, never while visible (§4). The lifetime is a constant for now;
// Settings will replace this single reader (default 3 h, §12).
const TEMPORARY_LIFETIME_MS = 3 * 60 * 60 * 1000;

export function temporaryLifetimeMs(): number {
  return TEMPORARY_LIFETIME_MS;
}

// Pinned Tabs currently away from their Anchor — the Tabs a background reset
// returns home.
export function excursionsToReset(tabs: readonly Tab[]): PinnedTab[] {
  return tabs.filter(
    (tab): tab is PinnedTab =>
      tab.kind === "pinned" && tab.browse.location !== tab.anchorPath,
  );
}

// Temporary Tabs idle beyond their lifetime, excluding the active Tab which is
// never removed (it just became visible). Idle is measured from last activation.
export function expiredTemporaryIds(
  tabs: readonly Tab[],
  activeId: TabId,
  nowMs: number,
): TabId[] {
  const cutoff = nowMs - temporaryLifetimeMs();
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
