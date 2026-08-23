# TypeScript discipline

Enforcement lives in the environment: `tsconfig.base.json` (maximal compiler strictness, including `noUncheckedIndexedAccess` and `exactOptionalPropertyTypes`) and `eslint.config.js` (`strict-type-checked` plus a ban on type assertions). Don't weaken those files to make code compile; change the code. This document carries the patterns the tooling cannot infer — reach for them by default, not as a last resort.

## Make invalid states unrepresentable

Model state as a discriminated union, one variant per real state, data attached to the variant that owns it:

```ts
type LoadState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "error"; error: AppError }
  | { status: "ready"; items: Item[] };
```

A bag of optional fields (`{ isLoading: boolean; error?: E; data?: T }`) admits meaningless combinations; the union makes the compiler prove `items` is only touched in `ready`.

## Match exhaustively

Consume unions with `ts-pattern`:

```ts
import { match } from "ts-pattern";

const view = match(state)
  .with({ status: "idle" }, () => null)
  .with({ status: "loading" }, () => <Spinner />)
  .with({ status: "error" }, ({ error }) => <ErrorRow error={error} />)
  .with({ status: "ready" }, ({ items }) => <List items={items} />)
  .exhaustive();
```

`.exhaustive()` turns a forgotten variant into a type error. A `switch` on the discriminant is acceptable in hot paths; the `switch-exhaustiveness-check` lint rule covers it, so no `default` branch — let the rule prove completeness.

## Parse, don't validate

Data is untyped until a zod schema has parsed it. Boundaries where this is mandatory: every Tauri `invoke` result, every Tauri event payload, persisted state read back from disk, and anything else that crosses out of TypeScript. Inside the boundary, code takes the parsed type and never re-checks.

Identifiers that must not mix (paths, Tab ids, …) get branded types, produced only by their schema or parse function:

```ts
const TabId = z.uuid().brand<"TabId">();
type TabId = z.infer<typeof TabId>;
```

## Errors are values

Fallible domain operations return `neverthrow`'s `Result<T, E>` with a union of literal error codes as `E`, not thrown exceptions — exceptions are invisible to the type system. `throw` is reserved for programmer errors (broken invariants) that should crash loudly. A rejected promise from Tauri `invoke` is converted to a `Result` at the boundary, in one wrapper, not at every call site.

## Assertions are `unsafe`

The lint config bans `as` entirely. When an assertion is genuinely sound and unavoidable, disable the rule for that one line with a comment stating why the assertion holds. If the justification is hard to write, the assertion is wrong.
