# Beeline

Repo for Beeline (working name for version 1; formerly Visual Files) — a personal, fast macOS file browser meant to replace Finder for everyday file work. The production implementation lives at the repo root; `docs/SPEC.md` is the implementation-ready specification, backed by ADRs. The planning-era demos and prototypes are deleted from the working tree and remain at git tag `planning-end`.

- Canonical domain language: `CONTEXT.md` at the repo root (glossary only, no implementation details).
- Decisions: `docs/adr/`.
- The wayfinder map and its decision tickets live in this repo's GitHub Issues. The pre-migration markdown tracker is archived under `.scratch/raycast-visual-files/archive/`.

## Agent skills

- Issue tracker: GitHub Issues — conventions in `docs/agents/issue-tracker.md`.
- Domain docs: single context — `CONTEXT.md` and `docs/adr/` at the repo root; consumer rules in `docs/agents/domain.md`.
- TypeScript: read `docs/agents/typescript.md` before writing or reviewing any TypeScript — required patterns (discriminated unions, exhaustive match, parse-don't-validate, Result) on top of the strict tsconfig and lint.
