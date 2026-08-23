import { listen } from "@tauri-apps/api/event";
import { useEffect, useState, type ReactElement } from "react";
import { z } from "zod";

import { reportShellError } from "../shell";

const CENTER_GUIDE_EVENT = "beeline://center-guide";
const HIDE_AFTER_MS = 400;

const nearSchema = z.boolean();

// SPEC §2: while the window is dragged near the centered position, thin guide
// lines through the window's own center axes mark the snap target. The Rust
// side emits on every near tick, so the hide timeout re-arms while the drag
// continues and clears the lines shortly after it ends.
export function CenterGuides(): ReactElement | null {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    let timer: number | undefined;
    let unlisten: (() => void) | null = null;
    let disposed = false;

    listen(CENTER_GUIDE_EVENT, (event) => {
      const parsed = nearSchema.safeParse(event.payload);
      if (!parsed.success) {
        reportShellError({
          code: "invalid-boundary-payload",
          operation: CENTER_GUIDE_EVENT,
          cause: parsed.error,
        });
        return;
      }
      setVisible(parsed.data);
      if (timer !== undefined) {
        window.clearTimeout(timer);
        timer = undefined;
      }
      if (parsed.data) {
        timer = window.setTimeout(() => {
          setVisible(false);
        }, HIDE_AFTER_MS);
      }
    }).then(
      (dispose) => {
        if (disposed) {
          dispose();
        } else {
          unlisten = dispose;
        }
      },
      (cause: unknown) => {
        reportShellError({
          code: "tauri-request-failed",
          operation: `listen:${CENTER_GUIDE_EVENT}`,
          cause,
        });
      },
    );

    return () => {
      disposed = true;
      if (timer !== undefined) {
        window.clearTimeout(timer);
      }
      if (unlisten !== null) {
        unlisten();
      }
    };
  }, []);

  if (!visible) {
    return null;
  }
  return (
    <div className="pointer-events-none absolute inset-0 z-50">
      <div className="absolute left-1/2 top-0 h-full w-px bg-blue-400/60" />
      <div className="absolute top-1/2 left-0 h-px w-full bg-blue-400/60" />
    </div>
  );
}
