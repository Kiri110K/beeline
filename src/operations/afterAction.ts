// The After Action defaults table (SPEC §12): after certain actions the window hides,
// after others it stays open. This is the single lookup the whole app consults; Settings
// replaces the fixed table with a user-editable one later. Keeping it a pure function
// keeps the policy testable and out of the wiring.

export type AfterActionId =
  | "open_file"
  | "open_terminal"
  | "open_editor"
  | "copy_path"
  | "copy_file"
  | "trash"
  | "delete_permanently"
  | "enter_directory"
  | "navigation"
  | "paste"
  | "move_paste"
  | "rename"
  | "new_folder"
  | "reveal"
  | "open_in_new_tab";

export type AfterEffect = "hide" | "keep";

// SPEC §12 defaults: hide after Open File, Open in Terminal/Editor, Copy Path, Copy File;
// keep the window open after everything else (Trash, Enter Directory, navigation, …).
const HIDE_AFTER: ReadonlySet<AfterActionId> = new Set<AfterActionId>([
  "open_file",
  "open_terminal",
  "open_editor",
  "copy_path",
  "copy_file",
]);

export function afterEffectFor(action: AfterActionId): AfterEffect {
  return HIDE_AFTER.has(action) ? "hide" : "keep";
}
