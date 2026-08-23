import type { ReactElement } from "react";

import { strings } from "../strings";
import type { Tabs } from "../tabs/useTabs";

// The Status Strip (SPEC §8, §10, §13): the persistent slim zone that shows active batch
// jobs (label + n/total) and, on failure, a problem list — concrete per-item causes, most
// recent first, each dismissible. It is the product's only async-feedback surface: no
// toasts, no banners. Empty when nothing is running and nothing has failed.
export function StatusStrip({ controller }: { controller: Tabs }): ReactElement {
  const { ops, dismissProblem } = controller;
  const { jobs, problems } = ops;
  const empty = jobs.length === 0 && problems.length === 0;

  return (
    <div
      aria-label={strings.zones.statusStrip}
      className="max-h-24 shrink-0 overflow-auto border-t border-neutral-800 text-xs"
    >
      {empty ? (
        <div className="h-6" aria-hidden="true" />
      ) : (
        <div className="divide-y divide-neutral-800/60">
          {jobs.map((job) => (
            <div
              key={job.jobId}
              className="flex items-center gap-2 px-3 py-1 text-neutral-300"
            >
              <span className="truncate">{job.label}</span>
              <span className="tabular-nums text-neutral-500">
                {job.done}/{job.total}
              </span>
            </div>
          ))}
          {problems.map((problem) => (
            <div
              key={problem.id}
              className="flex items-center gap-2 px-3 py-1 text-red-300"
            >
              <span className="min-w-0 flex-1 truncate">{problem.cause}</span>
              <button
                type="button"
                aria-label={strings.operations.status.dismiss}
                onClick={() => {
                  dismissProblem(problem.id);
                }}
                className="shrink-0 rounded px-1 text-red-300/70 hover:text-red-200"
              >
                ×
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
