import type { JobId, OperationFailure } from "./ipc";

// How the Action Menu was opened (SPEC §5; telemetry `action_menu_opened {via}`).
export type MenuVia = "cmd_k" | "row_button" | "context";

// A live batch job shown in the Status Strip (SPEC §8, §10): a label and n/total.
export interface JobView {
  jobId: JobId;
  label: string;
  done: number;
  total: number;
}

// One failure in the Status Strip's problem list (SPEC §8, §13): a concrete cause,
// dismissible, most recent first. `id` is app-local so React can key and dismiss it.
export interface ProblemView {
  id: number;
  path: string;
  cause: string;
}

// One entry in the open Action Menu (SPEC §5). The items are derived from the live
// selection each render; `run` closes over the controller, so this lives with the menu
// rather than in the reducer. `disabled` greys an inapplicable action (e.g. Paste with an
// empty clipboard) without removing it.
export interface MenuItem {
  id: string;
  label: string;
  disabled: boolean;
  run: () => void;
}

// The open Action Menu. Content is derived at render from the live selection, so this
// carries only how and where it opened plus the keyboard-focused row; `x`/`y` are null for
// a keyboard-opened menu.
export interface MenuView {
  via: MenuVia;
  x: number | null;
  y: number | null;
  focusedIndex: number;
}

// An inline rename in progress (SPEC §8): the target path, the name the field seeds with,
// and the engine's collision message shown inline (null until one occurs).
export interface RenameView {
  path: string;
  initialName: string;
  error: string | null;
}

// The always-on Delete Permanently confirmation (SPEC §8): the paths awaiting the user's
// Enter/Escape. No "don't ask again".
export interface ConfirmView {
  paths: string[];
}

export interface OpsState {
  // In-app clipboard file references (Finder model, SPEC §8): the copied paths, or null.
  clipboard: string[] | null;
  jobs: JobView[];
  problems: ProblemView[];
  nextProblemId: number;
  menu: MenuView | null;
  rename: RenameView | null;
  confirm: ConfirmView | null;
}

export const initialOpsState: OpsState = {
  clipboard: null,
  jobs: [],
  problems: [],
  nextProblemId: 1,
  menu: null,
  rename: null,
  confirm: null,
};

export type OpsAction =
  | { type: "setClipboard"; paths: string[] | null }
  | { type: "jobStarted"; jobId: JobId; label: string; total: number }
  | { type: "jobProgress"; updates: { jobId: JobId; done: number; total: number }[] }
  | { type: "jobFinished"; jobId: JobId; failures: OperationFailure[] }
  | { type: "pushProblem"; path: string; cause: string }
  | { type: "dismissProblem"; id: number }
  | {
      type: "openMenu";
      via: MenuVia;
      x: number | null;
      y: number | null;
      focusedIndex: number;
    }
  | { type: "menuFocus"; index: number }
  | { type: "closeMenu" }
  | { type: "startRename"; path: string; initialName: string }
  | { type: "renameError"; error: string }
  | { type: "cancelRename" }
  | { type: "openConfirm"; paths: string[] }
  | { type: "closeConfirm" };

// Prepend one problem, minting its id. Shared by batch failures and single-action errors.
function withProblem(
  state: OpsState,
  path: string,
  cause: string,
): OpsState {
  const problem: ProblemView = { id: state.nextProblemId, path, cause };
  return {
    ...state,
    problems: [problem, ...state.problems],
    nextProblemId: state.nextProblemId + 1,
  };
}

export function opsReducer(state: OpsState, action: OpsAction): OpsState {
  switch (action.type) {
    case "setClipboard":
      return { ...state, clipboard: action.paths };

    case "jobStarted":
      return {
        ...state,
        jobs: [
          ...state.jobs,
          { jobId: action.jobId, label: action.label, done: 0, total: action.total },
        ],
      };

    case "jobProgress": {
      if (action.updates.length === 0) {
        return state;
      }
      const byId = new Map(action.updates.map((u) => [u.jobId, u]));
      return {
        ...state,
        jobs: state.jobs.map((job) => {
          const update = byId.get(job.jobId);
          return update === undefined
            ? job
            : { ...job, done: update.done, total: update.total };
        }),
      };
    }

    case "jobFinished": {
      const jobs = state.jobs.filter((job) => job.jobId !== action.jobId);
      // Newest failures first, so a fresh problem always lands at the top of the list.
      let next: OpsState = { ...state, jobs };
      for (const failure of action.failures) {
        next = withProblem(next, failure.path, failure.cause);
      }
      return next;
    }

    case "pushProblem":
      return withProblem(state, action.path, action.cause);

    case "dismissProblem":
      return {
        ...state,
        problems: state.problems.filter((problem) => problem.id !== action.id),
      };

    case "openMenu":
      return {
        ...state,
        menu: {
          via: action.via,
          x: action.x,
          y: action.y,
          focusedIndex: action.focusedIndex,
        },
      };

    case "menuFocus":
      return state.menu === null
        ? state
        : { ...state, menu: { ...state.menu, focusedIndex: action.index } };

    case "closeMenu":
      return { ...state, menu: null };

    case "startRename":
      return {
        ...state,
        rename: { path: action.path, initialName: action.initialName, error: null },
      };

    case "renameError":
      return state.rename === null
        ? state
        : { ...state, rename: { ...state.rename, error: action.error } };

    case "cancelRename":
      return { ...state, rename: null };

    case "openConfirm":
      return { ...state, confirm: { paths: action.paths } };

    case "closeConfirm":
      return { ...state, confirm: null };
  }
}
