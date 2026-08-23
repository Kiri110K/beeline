import { useCallback, useEffect, useReducer, useRef } from "react";

import { homeDirectory, listLocation } from "../location/ipc";
import type { Item } from "../location/schema";
import { recordTelemetry, reportShellError } from "../shell";
import { openPath, primaryActionFor } from "./actions";
import {
  browseReducer,
  hasSelectionBeyondFocus,
  initialBrowseState,
  type BrowseState,
  type NavKind,
  type SelectMode,
} from "./state";

export interface Browse {
  state: BrowseState;
  select: (index: number, mode: SelectMode) => void;
  activate: (item: Item) => void;
  onScrollTop: (top: number) => void;
}

function isEditableTarget(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.tagName === "INPUT" || target.isContentEditable)
  );
}

export function useBrowse(): Browse {
  const [state, dispatch] = useReducer(browseReducer, initialBrowseState);

  // Latest state for event handlers that must not close over a stale snapshot.
  const stateRef = useRef(state);
  useEffect(() => {
    stateRef.current = state;
  }, [state]);

  // Current scroll offset, mirrored from the table so a navigation can record
  // the outgoing Location's scroll into history without re-rendering on scroll.
  const scrollTopRef = useRef(0);
  const onScrollTop = useCallback((top: number) => {
    scrollTopRef.current = top;
  }, []);

  // Monotonic request id: only the newest navigation's result may commit.
  const seqRef = useRef(0);

  const navigate = useCallback((target: string, nav: NavKind) => {
    const seq = seqRef.current + 1;
    seqRef.current = seq;
    const originScrollTop = scrollTopRef.current;
    void listLocation(target).match(
      (response) => {
        if (seq !== seqRef.current) {
          return;
        }
        dispatch({
          type: "listed",
          location: response.path,
          items: response.items,
          nav,
          originScrollTop,
        });
      },
      (error) => {
        if (seq !== seqRef.current) {
          return;
        }
        dispatch({ type: "failed", location: target, error });
      },
    );
  }, []);

  const openFile = useCallback((path: string) => {
    void openPath(path).match(
      () => {
        // Telemetry fires only after the opener call succeeds.
        void recordTelemetry("file_opened", { path }).match(
          () => undefined,
          reportShellError,
        );
      },
      reportShellError,
    );
  }, []);

  const activate = useCallback(
    (item: Item) => {
      const action = primaryActionFor(item);
      switch (action.kind) {
        case "enter":
          navigate(action.path, "enter");
          break;
        case "open":
          openFile(action.path);
          break;
      }
    },
    [navigate, openFile],
  );

  const activateFocused = useCallback(() => {
    const current = stateRef.current;
    if (current.load.status !== "ready") {
      return;
    }
    const focused = current.load.items[current.focusedIndex];
    if (focused !== undefined) {
      activate(focused);
    }
  }, [activate]);

  const goBack = useCallback(() => {
    const { history } = stateRef.current;
    const entry = history[history.length - 1];
    if (entry === undefined) {
      return;
    }
    navigate(entry.location, "back");
  }, [navigate]);

  const goForward = useCallback(() => {
    const { future } = stateRef.current;
    const entry = future[future.length - 1];
    if (entry === undefined) {
      return;
    }
    navigate(entry.location, "forward");
  }, [navigate]);

  const select = useCallback((index: number, mode: SelectMode) => {
    dispatch({ type: "select", index, mode });
  }, []);

  // Start Location: the user's home directory, resolved by Rust.
  useEffect(() => {
    void homeDirectory().match(
      (home) => {
        navigate(home, "replace");
      },
      () => {
        dispatch({ type: "failed", location: "", error: { code: "io" } });
      },
    );
  }, [navigate]);

  // Browse-mode keyboard contract. Registered in the capture phase so the
  // Escape branch can preventDefault before shell.ts's bubble-phase hide runs.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      if (event.defaultPrevented || isEditableTarget(event.target)) {
        return;
      }

      // Cmd+[ / Cmd+] are back/forward, always available in Browse.
      if (event.metaKey) {
        if (event.key === "[") {
          event.preventDefault();
          goBack();
        } else if (event.key === "]") {
          event.preventDefault();
          goForward();
        }
        return;
      }

      const extend = event.shiftKey;

      // Ctrl+J/K mirror Down/Up exactly (same dispatch path).
      if (event.ctrlKey) {
        if (event.key === "j" || event.key === "J") {
          event.preventDefault();
          dispatch({ type: "focusDelta", delta: 1, extend });
        } else if (event.key === "k" || event.key === "K") {
          event.preventDefault();
          dispatch({ type: "focusDelta", delta: -1, extend });
        }
        return;
      }

      switch (event.key) {
        case "ArrowDown":
          event.preventDefault();
          dispatch({ type: "focusDelta", delta: 1, extend });
          break;
        case "ArrowUp":
          event.preventDefault();
          dispatch({ type: "focusDelta", delta: -1, extend });
          break;
        case "ArrowRight":
        case "Enter":
          event.preventDefault();
          activateFocused();
          break;
        case "ArrowLeft":
          event.preventDefault();
          goBack();
          break;
        case "Escape":
          // Non-empty selection collapses first; otherwise shell.ts hides.
          if (hasSelectionBeyondFocus(stateRef.current)) {
            event.preventDefault();
            dispatch({ type: "clearSelection" });
          }
          break;
      }
    }
    document.addEventListener("keydown", onKeyDown, { capture: true });
    return () => {
      document.removeEventListener("keydown", onKeyDown, { capture: true });
    };
  }, [activateFocused, goBack, goForward]);

  return { state, select, activate, onScrollTop };
}
