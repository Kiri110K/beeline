import { z } from "zod";

import { initialBrowseState, type BrowseState } from "../browse/state";

// A Tab id is a branded uuid so it can never be confused with a path or any
// other string; it is produced only by parsing a real uuid.
export const TabId = z.uuid().brand<"TabId">();
export type TabId = z.infer<typeof TabId>;

export function freshTabId(): TabId {
  return TabId.parse(crypto.randomUUID());
}

// A Tab is either temporary or pinned (CONTEXT.md). Both carry a single browsing
// state: for a Pinned Tab the browsing state sits at the Anchor until navigation
// moves it away, at which point the Tab is on a Pinned Excursion — a state we
// derive from the Location rather than store, so it can never disagree with what
// the Tab is actually showing.
export interface TemporaryTab {
  kind: "temporary";
  id: TabId;
  // Wall-clock ms of creation and of the last time this Tab was the active Tab;
  // the idle clock that expires the Tab (§4) is measured from the latter.
  createdAtMs: number;
  lastActivatedAtMs: number;
  // The Tab this one was created beside, so closing can return activation to it.
  originatorId: TabId | null;
  browse: BrowseState;
}

export interface PinnedTab {
  kind: "pinned";
  id: TabId;
  anchorPath: string;
  customName: string | null;
  browse: BrowseState;
}

export type Tab = TemporaryTab | PinnedTab;

export function makeTemporaryTab(args: {
  id: TabId;
  nowMs: number;
  originatorId: TabId | null;
}): TemporaryTab {
  return {
    kind: "temporary",
    id: args.id,
    createdAtMs: args.nowMs,
    lastActivatedAtMs: args.nowMs,
    originatorId: args.originatorId,
    browse: initialBrowseState,
  };
}

// The final path segment, used as a folder's display name. "/" for the root and
// the whole path when it has no separators.
export function folderName(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  if (trimmed === "") {
    return "/";
  }
  const slash = trimmed.lastIndexOf("/");
  return slash === -1 ? trimmed : trimmed.slice(slash + 1);
}

// A Pinned Tab is on a Pinned Excursion whenever its Location has moved away
// from its Anchor. Derived, never stored (see the note on `Tab`).
export function isOnExcursion(tab: Tab): boolean {
  return tab.kind === "pinned" && tab.browse.location !== tab.anchorPath;
}

export function isPinned(tab: Tab): tab is PinnedTab {
  return tab.kind === "pinned";
}

// Number of Pinned Tabs, which always occupy the front of the ordered list.
export function pinnedCount(tabs: readonly Tab[]): number {
  let count = 0;
  for (const tab of tabs) {
    if (tab.kind === "pinned") {
      count += 1;
    }
  }
  return count;
}
