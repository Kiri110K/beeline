import {
  useCallback,
  useEffect,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactElement,
  type ReactNode,
} from "react";
import { type ResultAsync } from "neverthrow";

import {
  AFTER_ACTION_IDS,
  type AfterActionId,
  type AfterEffect,
} from "../operations/afterAction";
import { resolveInstalledBundle } from "../operations/ipc";
import {
  EDITOR_BUNDLE_IDS,
  TERMINAL_BUNDLE_IDS,
  type Slot,
} from "../operations/settings";
import { reportShellError, type ShellError } from "../shell";
import { strings } from "../strings";
import type { SetSettingsOutcome } from "./ipc";
import {
  DEFAULT_JUNK_PATTERNS,
  LIFETIME_OPTIONS,
  lifetimeKey,
  shortcutFromEvent,
  shortcutLabel,
  type AliasEntry,
  type LifetimeSetting,
  type Settings,
} from "./schema";

// Friendly names for the known Terminal/Editor bundle ids (SPEC §8); anything else shows its
// raw bundle id so a hand-picked app is still legible.
const BUNDLE_LABELS: Record<string, string> = {
  "com.mitchellh.ghostty": "Ghostty",
  "com.googlecode.iterm2": "iTerm2",
  "com.apple.Terminal": "Terminal",
  "dev.zed.Zed": "Zed",
  "com.microsoft.VSCode": "VS Code",
};
function bundleLabel(id: string): string {
  return BUNDLE_LABELS[id] ?? id;
}

const s = strings.settings;

// The in-app Settings view (SPEC §12): a single-page form over the complete v1 inventory,
// keyboard-first (native inputs, natural tab order, Escape to close — handled by the Tabs
// controller) and mouse-complete. Edits are held in a local draft and persisted through the
// settings store: discrete controls commit on change, text fields on blur.
export function SettingsView({
  settings,
  save,
  onClose,
}: {
  settings: Settings;
  save: (next: Settings) => ResultAsync<SetSettingsOutcome, ShellError>;
  onClose: () => void;
}): ReactElement {
  const [draft, setDraft] = useState<Settings>(settings);
  const [recording, setRecording] = useState(false);
  const [shortcutError, setShortcutError] = useState<string | null>(null);
  // Which known apps are installed, for the slot pickers (SPEC §8 detection).
  const [installed, setInstalled] = useState<ReadonlySet<string>>(new Set());

  useEffect(() => {
    const candidates = [...TERMINAL_BUNDLE_IDS, ...EDITOR_BUNDLE_IDS];
    let cancelled = false;
    void Promise.all(
      candidates.map((id) =>
        resolveInstalledBundle([id]).match(
          (found) => (found === id ? id : null),
          (error) => {
            reportShellError(error);
            return null;
          },
        ),
      ),
    ).then((results) => {
      if (!cancelled) {
        const found: string[] = results.flatMap((id) => (id === null ? [] : [id]));
        setInstalled(new Set(found));
      }
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // Persist a full draft (SPEC §12): reflect it locally, then hand it to the store. A rejected
  // shortcut is rolled back both here and by the store's re-pull (SPEC §2).
  const commit = useCallback(
    (next: Settings): void => {
      setDraft(next);
      void save(next).match((outcome) => {
        if (!outcome.shortcutRegistered) {
          setShortcutError(s.shortcut.unavailable);
          setDraft((current) => ({
            ...current,
            globalShortcut: settings.globalShortcut,
          }));
        }
      }, reportShellError);
    },
    [save, settings.globalShortcut],
  );

  const captureShortcut = useCallback(
    (event: ReactKeyboardEvent): void => {
      event.preventDefault();
      event.stopPropagation();
      const spec = shortcutFromEvent(event);
      if (spec === null) {
        return; // A bare modifier press; keep waiting for the main key.
      }
      setRecording(false);
      if (!spec.control && !spec.alt && !spec.shift && !spec.meta) {
        setShortcutError(s.shortcut.noModifier);
        return;
      }
      setShortcutError(null);
      commit({ ...draft, globalShortcut: spec });
    },
    [commit, draft],
  );

  const setLifetime = (setting: LifetimeSetting): void => {
    commit({ ...draft, temporaryTabLifetime: setting });
  };

  const setAfterEffect = (id: AfterActionId, effect: AfterEffect): void => {
    commit({ ...draft, afterAction: { ...draft.afterAction, [id]: effect } });
  };

  const setSlot = (slot: Slot, bundleId: string): void => {
    commit(
      slot === "terminal"
        ? { ...draft, terminalBundleId: bundleId }
        : { ...draft, editorBundleId: bundleId },
    );
  };

  return (
    <div className="fixed inset-0 z-[60] flex flex-col bg-neutral-900 text-neutral-100">
      <header className="flex items-center justify-between border-b border-neutral-800 px-6 py-3">
        <h1 className="text-sm font-semibold">{s.title}</h1>
        <button
          type="button"
          onClick={onClose}
          className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500"
        >
          {s.done}
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-auto px-6 py-4">
        <div className="mx-auto flex max-w-2xl flex-col gap-6">
          {/* Global shortcut (§2) */}
          <Field label={s.shortcut.label} hint={s.shortcut.hint}>
            <button
              type="button"
              onClick={() => {
                setRecording(true);
                setShortcutError(null);
              }}
              onKeyDown={recording ? captureShortcut : undefined}
              className={`min-w-[140px] rounded border px-3 py-1 text-left font-mono ${
                recording
                  ? "border-blue-500 bg-neutral-800 text-blue-300"
                  : "border-neutral-600 bg-neutral-800"
              }`}
            >
              {recording
                ? s.shortcut.recording
                : shortcutLabel(draft.globalShortcut)}
            </button>
            {shortcutError !== null ? (
              <span className="ml-3 text-[12px] text-amber-400">{shortcutError}</span>
            ) : null}
          </Field>

          {/* Default entry point (§4) */}
          <Field label={s.entryPoint.label}>
            <div className="flex flex-col gap-2">
              <label className="flex items-center gap-2">
                <input
                  type="radio"
                  name="entryPoint"
                  checked={draft.defaultEntryPoint.kind === "recents"}
                  onChange={() => {
                    commit({ ...draft, defaultEntryPoint: { kind: "recents" } });
                  }}
                />
                {s.entryPoint.recents}
              </label>
              <label className="flex items-center gap-2">
                <input
                  type="radio"
                  name="entryPoint"
                  checked={draft.defaultEntryPoint.kind === "directory"}
                  onChange={() => {
                    const path =
                      draft.defaultEntryPoint.kind === "directory"
                        ? draft.defaultEntryPoint.path
                        : "";
                    setDraft({
                      ...draft,
                      defaultEntryPoint: { kind: "directory", path },
                    });
                  }}
                />
                {s.entryPoint.directory}
                <input
                  type="text"
                  disabled={draft.defaultEntryPoint.kind !== "directory"}
                  value={
                    draft.defaultEntryPoint.kind === "directory"
                      ? draft.defaultEntryPoint.path
                      : ""
                  }
                  placeholder={s.entryPoint.pathPlaceholder}
                  onChange={(event) => {
                    setDraft({
                      ...draft,
                      defaultEntryPoint: {
                        kind: "directory",
                        path: event.target.value,
                      },
                    });
                  }}
                  onBlur={() => {
                    commit(draft);
                  }}
                  className="flex-1 rounded border border-neutral-600 bg-neutral-800 px-2 py-1 disabled:opacity-40"
                />
              </label>
            </div>
          </Field>

          {/* Temporary tab lifetime (§4) */}
          <Field label={s.lifetime.label}>
            <select
              value={lifetimeKey(draft.temporaryTabLifetime)}
              onChange={(event) => {
                const option = LIFETIME_OPTIONS.find(
                  (candidate) => lifetimeKey(candidate) === event.target.value,
                );
                if (option !== undefined) {
                  setLifetime(option);
                }
              }}
              className="rounded border border-neutral-600 bg-neutral-800 px-2 py-1"
            >
              {LIFETIME_OPTIONS.map((option) => (
                <option key={lifetimeKey(option)} value={lifetimeKey(option)}>
                  {option.kind === "never"
                    ? s.lifetime.never
                    : s.lifetime.minutes(option.minutes)}
                </option>
              ))}
            </select>
          </Field>

          {/* Primary actions (§5) */}
          <Field label={s.primaryAction.directoryLabel}>
            <select
              value={draft.primaryActionDirectory}
              onChange={(event) => {
                commit({
                  ...draft,
                  primaryActionDirectory:
                    event.target.value === "menu" ? "menu" : "enter",
                });
              }}
              className="rounded border border-neutral-600 bg-neutral-800 px-2 py-1"
            >
              <option value="enter">{s.primaryAction.enter}</option>
              <option value="menu">{s.primaryAction.menu}</option>
            </select>
          </Field>
          <Field label={s.primaryAction.fileLabel}>
            <select
              value={draft.primaryActionFile}
              onChange={(event) => {
                commit({
                  ...draft,
                  primaryActionFile:
                    event.target.value === "menu" ? "menu" : "open",
                });
              }}
              className="rounded border border-neutral-600 bg-neutral-800 px-2 py-1"
            >
              <option value="open">{s.primaryAction.open}</option>
              <option value="menu">{s.primaryAction.menu}</option>
            </select>
          </Field>

          {/* After Action table (§12) */}
          <section>
            <h2 className="mb-2 text-sm font-medium">{s.afterAction.heading}</h2>
            <div className="flex flex-col gap-1">
              {AFTER_ACTION_IDS.map((id) => (
                <div
                  key={id}
                  className="flex items-center justify-between border-b border-neutral-800 py-1"
                >
                  <span className="text-neutral-300">
                    {s.afterAction.labels[id]}
                  </span>
                  <div className="flex gap-4">
                    <label className="flex items-center gap-1">
                      <input
                        type="radio"
                        name={`after-${id}`}
                        checked={draft.afterAction[id] === "hide"}
                        onChange={() => {
                          setAfterEffect(id, "hide");
                        }}
                      />
                      {s.afterAction.hide}
                    </label>
                    <label className="flex items-center gap-1">
                      <input
                        type="radio"
                        name={`after-${id}`}
                        checked={draft.afterAction[id] === "keep"}
                        onChange={() => {
                          setAfterEffect(id, "keep");
                        }}
                      />
                      {s.afterAction.keep}
                    </label>
                  </div>
                </div>
              ))}
            </div>
          </section>

          {/* Preview panel (§9) */}
          <Field label={s.preview.label}>
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={draft.previewPanelVisible}
                onChange={(event) => {
                  commit({ ...draft, previewPanelVisible: event.target.checked });
                }}
              />
              {s.preview.on}
            </label>
          </Field>

          {/* Application slots (§8) */}
          <section>
            <h2 className="mb-2 text-sm font-medium">{s.slots.heading}</h2>
            <div className="flex flex-col gap-2">
              <SlotPicker
                label={s.slots.terminalLabel}
                slot="terminal"
                value={draft.terminalBundleId}
                candidates={TERMINAL_BUNDLE_IDS}
                installed={installed}
                onPick={setSlot}
              />
              <SlotPicker
                label={s.slots.editorLabel}
                slot="editor"
                value={draft.editorBundleId}
                candidates={EDITOR_BUNDLE_IDS}
                installed={installed}
                onPick={setSlot}
              />
            </div>
          </section>

          {/* Alias Dictionary (§6) */}
          <ListEditor heading={s.aliases.heading} hint={s.aliases.hint}>
            {draft.aliases.map((alias, index) => (
              <div key={index} className="flex items-center gap-2">
                <input
                  type="text"
                  value={alias.word}
                  placeholder={s.aliases.wordPlaceholder}
                  onChange={(event) => {
                    setDraft({
                      ...draft,
                      aliases: replaceAt(draft.aliases, index, {
                        ...alias,
                        word: event.target.value,
                      }),
                    });
                  }}
                  onBlur={() => {
                    commit(draft);
                  }}
                  className="w-40 rounded border border-neutral-600 bg-neutral-800 px-2 py-1"
                />
                <input
                  type="text"
                  value={alias.path}
                  placeholder={s.aliases.pathPlaceholder}
                  onChange={(event) => {
                    setDraft({
                      ...draft,
                      aliases: replaceAt(draft.aliases, index, {
                        ...alias,
                        path: event.target.value,
                      }),
                    });
                  }}
                  onBlur={() => {
                    commit(draft);
                  }}
                  className="flex-1 rounded border border-neutral-600 bg-neutral-800 px-2 py-1"
                />
                <button
                  type="button"
                  onClick={() => {
                    commit({ ...draft, aliases: removeAt(draft.aliases, index) });
                  }}
                  className="rounded px-2 py-1 text-neutral-400 hover:text-neutral-200"
                >
                  {s.aliases.remove}
                </button>
              </div>
            ))}
            <button
              type="button"
              onClick={() => {
                const entry: AliasEntry = { word: "", path: "" };
                commit({ ...draft, aliases: [...draft.aliases, entry] });
              }}
              className="self-start rounded border border-neutral-600 px-3 py-1 hover:bg-neutral-800"
            >
              {s.aliases.add}
            </button>
          </ListEditor>

          {/* Junk patterns (§6) */}
          <ListEditor heading={s.junk.heading} hint={s.junk.hint}>
            {draft.junkPatterns.map((pattern, index) => (
              <div key={index} className="flex items-center gap-2">
                <input
                  type="text"
                  value={pattern}
                  placeholder={s.junk.placeholder}
                  onChange={(event) => {
                    setDraft({
                      ...draft,
                      junkPatterns: replaceAt(
                        draft.junkPatterns,
                        index,
                        event.target.value,
                      ),
                    });
                  }}
                  onBlur={() => {
                    commit(draft);
                  }}
                  className="flex-1 rounded border border-neutral-600 bg-neutral-800 px-2 py-1"
                />
                <button
                  type="button"
                  onClick={() => {
                    commit({
                      ...draft,
                      junkPatterns: removeAt(draft.junkPatterns, index),
                    });
                  }}
                  className="rounded px-2 py-1 text-neutral-400 hover:text-neutral-200"
                >
                  {s.junk.remove}
                </button>
              </div>
            ))}
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => {
                  commit({ ...draft, junkPatterns: [...draft.junkPatterns, ""] });
                }}
                className="self-start rounded border border-neutral-600 px-3 py-1 hover:bg-neutral-800"
              >
                {s.junk.add}
              </button>
              <button
                type="button"
                onClick={() => {
                  commit({
                    ...draft,
                    junkPatterns: [...DEFAULT_JUNK_PATTERNS],
                  });
                }}
                className="self-start rounded px-3 py-1 text-neutral-400 hover:text-neutral-200"
              >
                {s.junk.reset}
              </button>
            </div>
          </ListEditor>
        </div>
      </div>
    </div>
  );
}

// A labelled form row: a left-aligned label with an optional hint, and its control(s).
function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}): ReactElement {
  return (
    <section>
      <div className="mb-1 text-sm font-medium">{label}</div>
      {hint !== undefined ? (
        <p className="mb-2 text-[12px] text-neutral-500">{hint}</p>
      ) : null}
      <div className="flex flex-wrap items-center">{children}</div>
    </section>
  );
}

// A titled list section (aliases, junk) with a hint above its rows.
function ListEditor({
  heading,
  hint,
  children,
}: {
  heading: string;
  hint: string;
  children: ReactNode;
}): ReactElement {
  return (
    <section>
      <h2 className="mb-1 text-sm font-medium">{heading}</h2>
      <p className="mb-2 text-[12px] text-neutral-500">{hint}</p>
      <div className="flex flex-col gap-2">{children}</div>
    </section>
  );
}

// One application slot picker (SPEC §8): a <select> over the known apps, marking those that
// are not installed so the missing-app policy is visible in place.
function SlotPicker({
  label,
  slot,
  value,
  candidates,
  installed,
  onPick,
}: {
  label: string;
  slot: Slot;
  value: string | null;
  candidates: readonly string[];
  installed: ReadonlySet<string>;
  onPick: (slot: Slot, bundleId: string) => void;
}): ReactElement {
  // Include the current value even if it is not a known candidate (a hand-picked app).
  const options =
    value !== null && !candidates.includes(value)
      ? [value, ...candidates]
      : [...candidates];
  return (
    <div className="flex items-center justify-between">
      <span className="text-neutral-300">{label}</span>
      <select
        value={value ?? ""}
        onChange={(event) => {
          onPick(slot, event.target.value);
        }}
        className="rounded border border-neutral-600 bg-neutral-800 px-2 py-1"
      >
        {value === null ? (
          <option value="" disabled>
            {strings.settings.slots.none}
          </option>
        ) : null}
        {options.map((id) => (
          <option key={id} value={id}>
            {bundleLabel(id)}
            {installed.has(id) ? "" : ` (${strings.settings.slots.notInstalled})`}
          </option>
        ))}
      </select>
    </div>
  );
}

// Replace one element of a readonly array without mutating it.
function replaceAt<T>(items: readonly T[], index: number, value: T): T[] {
  return items.map((item, current) => (current === index ? value : item));
}

// Remove one element of a readonly array without mutating it.
function removeAt<T>(items: readonly T[], index: number): T[] {
  return items.filter((_item, current) => current !== index);
}
