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

## Состояние после ночного прогона 23–24.08

- Ночные фиксы (коммит 7f37bec, запушен): многословный поиск (репро Кирилла
  «процедура приемки» находится), окно на всех Spaces и поверх фуллскрина,
  минимальное меню (дефолтное съедало Cmd+W до вебвью; совсем без меню
  приложение не может стать активным), корзина через NSFileManager (Finder-
  AppleScript вис за TCC-диалогом; Put Back — компромисс в #28).
- Тикеты #24–#29, #32 закрыты с вердиктами; #30 открыт (визуальный проход
  Settings за Кириллом), #31 открыт (перф-таблица и отклонения в тикете).
- Утром Кириллу: ответить на TCC-диалог «T3 → Finder» если снова всплывёт
  (Don't Allow безопасно), физически нажать Ctrl+Opt+Cmd+F (проверка
  активации реальной клавишей), 2 минуты глазами: Settings (Cmd+,),
  Cmd+W на Табе, Ctrl+Tab, драг Табов, пин через контекст-меню.
- Известный перф-гэп: холодный многословный кириллический запрос 315 мс
  (бюджет 50); план — кэш lowercase-путей по DirId (#31).

## Состояние на последнее обновление (история)

- ВЕСЬ код v1 написан и запушен: #24 Shell (закрыт), #25 Tabs/Browse,
  #26 Name Index+ранкер+Search UI, #27 Recents (mdfind -attr, 142 мс),
  #28 операции+Action Menu+Status Strip, #29 Quick Look (objc2 мост,
  orderFront) + Preview Panel, #30 Settings+first-run, #32 направляющие
  (закрыт). Свежая сборка стоит в /Applications/Beeline.app.
- Осталось: (а) пакетный GUI-смоук — вотчер простоя ≥15 мин уже взведён,
  скрипт /tmp/beeline-gui-smoke.sh (Табы, поиск, Quick Look, скриншоты,
  замер поиска на живом 5М-индексе); по его итогам закрыть #25–#30;
  риск для проверки: рисуется ли неключевая QLPreviewPanel (orderFront) —
  если нет, фолбэк responder-chain отдельным тикетом;
  (б) #31 перф-валидация: все бюджеты §10 на release + сквозной смоук §16.
- Замеры уже снятые: тёплый показ→отрисовка 22.6 мс (бюджет 50), ручной
  старт 590 мс (бюджет 800), прогрев с 5М-индексом 1.8 с (бюджет 2),
  листинги 0–4 мс (бюджет 100), Recents 142 мс (бюджет 150), поиск на
  синтетике 1М — 1.5–20.6 мс (бюджет 50; на живых 5М не мерено — вотч).

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
- Прогоны `claude -p` могут умирать от обрыва сети (Кирилл: сеть нестабильна,
  это норма — caffeinate не нужен). Лечение: `git status/diff` — если кусок
  недописан, перезапустить его тем же промптом (файлы промптов —
  /tmp/beeline-t*-prompt.md) или откатить и перезапустить с нуля.
- `opener:default` не даёт open-path: для открытия файлов нужен
  `opener:allow-open-path` в src-tauri/capabilities/default.json.

## Как проверить, что сделанное работает

```sh
cd /Users/kiri110k/lab/beeline
pnpm typecheck && pnpm lint && cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
pnpm tauri build   # подписывается идентити «Beeline Dev Signing» (login keychain)
rm -rf /Applications/Beeline.app && ditto src-tauri/target/release/bundle/macos/Beeline.app /Applications/Beeline.app
open /Applications/Beeline.app   # запускать ТОЛЬКО из /Applications — там FDA-грант
# шорткат-тоггл и Escape — см. osascript выше; тайминги — в telemetry.ndjson
```

## GUI-тесты и машина Кирилла

- Синтетические клавиши (osascript) слать ТОЛЬКО когда машина свободна:
  HIDIdleTime ≥ 120 с И фронтмост = beeline (проверять перед каждой пачкой).
  Прецедент 23.08: Cmd+T/Cmd+W улетали в активные окна Кирилла.
- Активировать Beeline AppleScript'ом нельзя (Accessory) — фокусировать его
  же глобальным шорткатом (key code 3 + ctrl/opt/cmd).
- TCC: подпись стабильная, FDA-грант на /Applications/Beeline.app ставит
  Кирилл один раз; до гранта запуски дают попапы — не запускать без него.
- Первичный кроулинг Name Index — Utility QoS (background душится ядром,
  84 записи/с; фикс в crawl.rs::set_crawl_qos, отклонение от §10 в тикете #26).
