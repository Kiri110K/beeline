# HANDOFF — реализация Beeline v1

Обновляется после каждого закрытого куска работы. Читатель — новая сессия
Claude Code на этом Маке, без доступа к прошлой.

## Задача словами Кирилла

Довести реализацию Beeline v1 «до конца, чтобы всё было сделано» по спеке
`/Users/kiri110k/lab/beeline/docs/SPEC.md`. Кодит Опус, приоритетно версия 4.8
(`claude --model claude-opus-4-8 -p`, codex с имплементации снят — Кирилл
считает его код некрасивым). Fable решает, ревьюит и проверяет живьём.

## Источник правды по прогрессу

GitHub-трекер: https://github.com/Kiri110K/beeline/issues/23 (родитель).
Тикеты #24–#32; закрытый = сделан и проверен, у закрытого есть
комментарий-вердикт. Открытый с assignee = был в работе; смотри его
комментарии и `git log` — что уже закоммичено.

## Состояние на последнее обновление

- Сделано: #24 Shell (коммит 45e228b) — окно, глобальный шорткат-тоггл
  Ctrl+Opt+Cmd+F, Accessory, автостарт с `--hidden`, снап к центру,
  NDJSON-телеметрия, Escape. Замеры release: тёплый показ→отрисовка 22.6 мс
  (бюджет 50), ручной старт→отрисовка 590 мс (бюджет 800).
- В работе: #25 Listing/Browse/Tabs — режется на куски: A) Rust-листинг +
  виртуализированная таблица, B) навигация/выделение/история, C) Табы с
  жизненным циклом и персистентностью, D) бюджеты+смоук.
- Дальше по зависимостям: #26 Name Index+поиск, #27 Recents, #28 операции,
  #29 Quick Look+Preview, #30 Settings+first-run, #32 направляющие линии,
  #31 перф-валидация. Финальная приёмка — смоук из SPEC §16.

## Процедура на каждый тикет (повторяемая)

1. `gh issue edit <n> --add-assignee @me` — клейм.
2. Опус 4.8 кодит куском: самодостаточный промпт (инварианты из SPEC §§ +
   docs/agents/typescript.md), запуск фоном:
   `claude --model claude-opus-4-8 --permission-mode acceptEdits
   --allowedTools "Bash(pnpm typecheck)" "Bash(pnpm lint)" "Bash(cargo clippy:*)"
   "Bash(git diff:*)" "Bash(git status:*)" -p "<промпт>"`.
3. Fable сам перегоняет: pnpm typecheck && pnpm lint && cargo clippy -D warnings;
   поведение — живьём на приложении (см. «Как проверить»).
4. Коммит + push после каждого проверенного куска (пуш = чекпойнт восстановления).
5. Закрытие тикета комментарием-вердиктом с замерами.
6. Обновить этот HANDOFF.md (можно в том же коммите следующего куска).

## Принятые решения и грабли

- Строгий TS обязателен: docs/agents/typescript.md; `as` запрещён линтом.
- pnpm build-скрипты: одобрение build-скриптов живёт в pnpm-workspace.yaml
  (`onlyBuiltDependencies`), поле pnpm в package.json не читается (pnpm 11).
- НЕ добавлять `git add -A` вслепую в каталогах с билд-артефактами — прецедент:
  в коммит попал бинарь Electron 163 МБ, GitHub отбил пуш, пришлось
  пересобирать коммиты (история чистая, тег planning-end цел).
- DMG-таргет отключён (targets: ["app"]) — bundle_dmg.sh падает в headless.
- Синтетический тест шортката: `osascript -e 'tell application "System Events"
  to key code 3 using {control down, option down, command down}'` (key code 3 = F).
- Телеметрия: `~/Library/Application Support/com.kiri110k.beeline/telemetry.ndjson`,
  перед замером файл удалить.
- cargo-зависимости качать до запуска песочных агентов (у них нет сети):
  `cargo add ... && cargo fetch --manifest-path src-tauri/Cargo.toml`.
- Длинные прогоны `claude -p` оборачивать в `caffeinate -i ...` — прецедент
  23.08: Мак уснул, прогон Опуса умер на середине («computer went to sleep»).
- `opener:default` не даёт open-path: для открытия файлов нужен
  `opener:allow-open-path` в src-tauri/capabilities/default.json.

## Как проверить, что сделанное работает

```sh
cd /Users/kiri110k/lab/beeline
pnpm typecheck && pnpm lint && cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
pnpm tauri build   # бандл: src-tauri/target/release/bundle/macos/Beeline.app
open src-tauri/target/release/bundle/macos/Beeline.app
# шорткат-тоггл и Escape — см. osascript выше; тайминги — в telemetry.ndjson
```
