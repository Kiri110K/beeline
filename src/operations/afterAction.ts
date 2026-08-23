// The After Action table (SPEC §12): after certain actions the window hides, after others
// it stays open. This is the single lookup the whole app consults; the live table comes from
// the Settings store (#30), so consumers pass it in rather than reading a fixed set here.

// The complete set of actions the After Action table covers, in the order the Settings view
// lists them. Quick Look is deliberately absent: it is a toggle that never hides the window
// (SPEC §9), so it has no meaningful Hide/Keep choice.
export const AFTER_ACTION_IDS = [
  "open_file",
  "open_terminal",
  "open_editor",
  "copy_path",
  "copy_file",
  "trash",
  "delete_permanently",
  "enter_directory",
  "navigation",
  "paste",
  "move_paste",
  "rename",
  "new_folder",
  "reveal",
  "open_in_new_tab",
] as const;

export type AfterActionId = (typeof AFTER_ACTION_IDS)[number];

export type AfterEffect = "hide" | "keep";

// The full table: every action maps to Hide or Keep. Parsed from the store with defaults
// filled, so consumers index it total-ly (no missing keys).
export type AfterActionTable = Record<AfterActionId, AfterEffect>;

// SPEC §12 defaults: hide after Open File, Open in Terminal/Editor, Copy Path, Copy File;
// keep the window open after everything else (Trash, Enter Directory, navigation, …).
export const DEFAULT_AFTER_ACTION: AfterActionTable = {
  open_file: "hide",
  open_terminal: "hide",
  open_editor: "hide",
  copy_path: "hide",
  copy_file: "hide",
  trash: "keep",
  delete_permanently: "keep",
  enter_directory: "keep",
  navigation: "keep",
  paste: "keep",
  move_paste: "keep",
  rename: "keep",
  new_folder: "keep",
  reveal: "keep",
  open_in_new_tab: "keep",
};

export function afterEffectFor(
  table: AfterActionTable,
  action: AfterActionId,
): AfterEffect {
  return table[action];
}
