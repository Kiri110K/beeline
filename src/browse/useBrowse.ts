import { useCallback, useEffect, useReducer, useRef } from "react";

import { homeDirectory, listLocation } from "../location/ipc";
import type { Item } from "../location/schema";
import {
  browseReducer,
  initialBrowseState,
  type BrowseState,
  type NavKind,
} from "./state";

export interface Browse {
  state: BrowseState;
  focusIndex: (index: number) => void;
  activate: (item: Item) => void;
}

export function useBrowse(): Browse {
  const [state, dispatch] = useReducer(browseReducer, initialBrowseState);

  // Latest state for event handlers that must not close over a stale snapshot.
  const stateRef = useRef(state);
  useEffect(() => {
    stateRef.current = state;
  }, [state]);

  // Monotonic request id: only the newest navigation's result may commit.
  const seqRef = useRef(0);

  const navigate = useCallback(
    (target: string, nav: NavKind, restorePath: string | null) => {
      const seq = seqRef.current + 1;
      seqRef.current = seq;
      void listLocation(target).match(
        (response) => {
          if (seq !== seqRef.current) {
            return;
          }
          dispatch({
            type: "listed",
            location: response.path,
            items: response.items,
            restorePath,
            nav,
          });
        },
        (error) => {
          if (seq !== seqRef.current) {
            return;
          }
          dispatch({ type: "failed", location: target, error });
        },
      );
    },
    [],
  );

  const activate = useCallback(
    (item: Item) => {
      // Files do nothing on activation this chunk; only directories are entered.
      if (item.isDirectory) {
        navigate(item.path, "forward", null);
      }
    },
    [navigate],
  );

  const goBack = useCallback(() => {
    const { history } = stateRef.current;
    const entry = history[history.length - 1];
    if (entry === undefined) {
      return;
    }
    navigate(entry.location, "back", entry.focusedPath);
  }, [navigate]);

  const focusIndex = useCallback((index: number) => {
    dispatch({ type: "focusIndex", index });
  }, []);

  // Start Location: the user's home directory, resolved by Rust.
  useEffect(() => {
    void homeDirectory().match(
      (home) => {
        navigate(home, "replace", null);
      },
      () => {
        dispatch({ type: "failed", location: "", error: { code: "io" } });
      },
    );
  }, [navigate]);

  // Browse-mode keyboard navigation (there is no Search yet).
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      if (event.defaultPrevented) {
        return;
      }
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.tagName === "INPUT" || target.isContentEditable)
      ) {
        return;
      }
      switch (event.key) {
        case "ArrowDown":
          event.preventDefault();
          dispatch({ type: "focusDelta", delta: 1 });
          break;
        case "ArrowUp":
          event.preventDefault();
          dispatch({ type: "focusDelta", delta: -1 });
          break;
        case "ArrowRight":
        case "Enter": {
          event.preventDefault();
          const current = stateRef.current;
          const focused =
            current.load.status === "ready"
              ? current.load.items[current.focusedIndex]
              : undefined;
          if (focused !== undefined) {
            activate(focused);
          }
          break;
        }
        case "ArrowLeft":
          event.preventDefault();
          goBack();
          break;
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [activate, goBack]);

  return { state, focusIndex, activate };
}
