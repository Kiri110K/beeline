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

## Состояние после Name Index v4 25.08

- Локально реализован прямой переход Name Index v3→v4 без миграции и обратной
  совместимости. Не-v4 файл считается отсутствующим, после чего обычный полный
  crawl сразу пишет v4.
- V4 хранит неизменяемую базу в read-only `mmap`: фиксированные записи каталогов
  и Items плюс единая UTF-8 arena. Изменения живут в маленьком overlay: tombstones,
  новые записи/каталоги и mtime overrides. Сохранение потоковое, с checksum,
  проверкой структуры, удалением недостижимых узлов и атомарной заменой файла.
- Первый физический app-pass нашёл реальную гонку: watcher применял FSEvents
  одновременно с initial crawl, мог повторно обходить большое поддерево и держать
  write-lock. Поиск зависал, а грязный heap на повторном запуске разрастался до
  12–14 ГБ. Теперь watcher регистрируется до crawl, складывает события в очередь и
  применяет их только после сборки mmap-базы. Реальный ignored FSEvents smoke это
  поведение проверяет.
- Повторный чистый app-pass дал PASS. Полный crawl: 5 150 527 записей за 101.757 с;
  первый v4-файл 309 859 773 байта. `процедура приемки` и wrong-layout
  `ghjwtlehf ghbtvrb` вернули по 2 результата за 64/58 мс сквозным временем и
  57/56 мс backend. После рестарта `index_loaded` пришёл за 982 мс от команды
  запуска, нового crawl не было; контрольный поиск занял 57/54 мс.
- После рестарта RSS около 419 MiB включает 308 MiB чистых mapped pages. Charged
  physical footprint стабилизировался на 68.6 MiB и не рос за четыре замера;
  peak 71.2 MiB. На первом запуске после compaction footprint стабилизировался на
  110.6 MiB. Отчёт:
  `/private/tmp/codex-computer-use.beeline-v4-fixed.08afZo/report.md`.
- Автопроверки зелёные: `pnpm test`, `pnpm typecheck`, `pnpm lint`, `pnpm build`,
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`;
  Rust: 78 passed, 6 ignored, 0 failed. Синтетика 5.08М: mutable-crawl запрос
  22 мс, mapped cold queries 7–29 мс. Живой mapped-файл грузится за 609–613 мс,
  медианы запросов 12–43 мс.
- Подписанный release-бандл собран в
  `/Users/kiri110k/lab/beeline/src-tauri/target/release/bundle/macos/Beeline.app`,
  но `/Applications/Beeline.app` не заменялся. Текущий реальный `home.idx` уже v4;
  предыдущий v4 сохранён в `/private/tmp/beeline-v4-pre-fix-backup.XTOjOr/home.idx`,
  исходный v3 — в `/private/tmp/beeline-v3-backup.zRJKFP/home.idx`.
- Изменения пока не закоммичены и не запушены. Пользовательская `.claude/`
  остаётся нетронутой и untracked. Прототип и замеры лежат в
  `prototypes/name-index-v4/`.

## Состояние после доведения Settings #30 25.08

- Settings #30 полностью доработан пятью коммитами `490c48f..cd6a1c5` и
  запушен в `main`.
  Frontend снова принимает и сохраняет backend-поле `junkSeedVersion`, а
  удалённые пользователем мигрированные Junk patterns не появляются повторно.
- Default Entry Point теперь проверяет путь в backend. Обычный файл и
  отсутствующий путь дают inline-ошибку, не сохраняются и не закрывают
  Settings; корректная папка сохраняется, выбор Recents можно восстановить.
- Настройки применяются только после подтверждённого backend-сохранения.
  Отказ при смене global shortcut больше не выглядит успешным; first-run текст
  честно отправляет за FDA в System Settings, а за автозапуском — в Login Items.
- Подключены все 15 After Action policies. Пустые Terminal/Editor slots
  автоматически заполняются установленными известными приложениями; сохранённый
  bundle ID отсутствующего приложения остаётся на месте и показывается как
  `not installed`.
- Дешёвые проверки зелёные: `pnpm test`, `pnpm typecheck`, `pnpm lint`,
  `pnpm build`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`. Rust: 76 passed, 5 ignored, 0 failed.
- После 386 секунд HID inactivity независимый GUI-прогон установленной
  `/Applications/Beeline.app` 0.1.0 дал PASS. Проверены нормальная загрузка
  Settings, Preview, Ghostty/Zed slots, ошибка для файла в Default Entry Point,
  сохранение временной папки, Navigation → Hide window и сохранение удаления
  Junk pattern. Все значения восстановлены, Beeline снова остановлен.
  Артефакты: `/private/tmp/codex-computer-use.beeline30.eahs14`.
- Shortcut-conflict и чистый first-run профиль живьём не трогали: первый мог
  перехватить ввод Кирилла, второй — затронуть системные настройки. Их логика
  проверена contract/unit tests; это явно отмечено в комментарии #30.
- Свежая release-сборка установлена в `/Applications/Beeline.app`. Предыдущая
  копия сохранена в
  `/private/tmp/beeline-issue30-backup.O56y1D/Beeline.app`.
- Обрезанный Action Menu у нижней строки вынесен в redesign backlog #39 без
  исходного скриншота: на нём были рабочие имена файлов и превью документа.
- #30 закрыт после публикации проверенных изменений. Пользовательская
  `.claude/` остаётся нетронутой и untracked.

## Состояние после battery/GUI-прохода #31 24.08

- #31 остаётся открытым. Локально реализована событийная battery policy через
  IOKit power-source notification, без polling: на батарее дорогой refresh
  Junk откладывается, после возврата внешнего питания очередь автоматически
  дренируется. Физический unplug/replug дал `source=battery`,
  `junk_rescan_deferred dirty_dirs=32`, затем `source=external junk_dirty=57`
  и `junk_rescan_finished reason=external_power dirty_dirs=23 duration_ms=18334`.
- Починен потерянный фокус после глобального шортката: пока окно показано,
  приложение временно переходит Accessory → Regular, на macOS 14 вызывается
  `NSApplication.activate()`, факт фокуса учитывает active app и key window.
  Физический прогон: Beeline стал frontmost, фокус установился с первой
  попытки примерно за 198 мс; после скрытия Dock icon исчезает.
- Большой каталог больше не блокирует первый кадр метаданными всех файлов:
  backend сортирует имена и сначала возвращает 64 записи, полные метаданные
  догружаются в фоне. Финальный release GUI-прогон на 50 000 файлов:
  initial 104 мс, первый кадр 123 мс (бюджет 150), полный список 853 мс в
  фоне. После 450 ArrowDown видны непрерывные строки 435–450 без пустот и
  tearing.
- Quick Look получает от frontend только текущий path, а AppKit-вызов уходит
  через отдельный FIFO dispatcher. Финальный прогон: возврат команды 6 мс
  (бюджет 50), нативное появление 66 мс. Process-local key monitor вернул
  навигацию при открытом QL: два ArrowDown дали два `quick_look_updated`, на
  снимках заголовок сменился `file_00450.txt` → `file_00452.txt`, Space закрыл
  панель. Артефакты: `/tmp/beeline-gui-monitor-pass.QmiPXO`.
- Автопроверки: 73 Rust-теста прошли, 5 тяжёлых/системных ignored; реальный
  ignored FSEvents smoke после явного сигнала готовности watcher прошёл три
  раза подряд (0.38–0.43 с). `pnpm typecheck`, `pnpm lint`, `cargo fmt
  --check`, `cargo clippy --all-targets -- -D warnings` и `git diff --check`
  зелёные.
- Подписанная release-сборка с GUI-фиксами установлена в
  `/Applications/Beeline.app`, PID финального прогона 50434. Предыдущая
  установочная копия: `/tmp/beeline-ql-monitor-prev.16bQcs/Beeline.app`.
  Idle у прежней release-сборки: средний CPU 0.043%, максимум 0.2%, +0.06 CPU
  seconds за 61 с. RSS около 61–72 MiB, но реальный footprint 525 MiB и peak
  813 MiB из-за compressed/swapped индекса на 5M записей — этот memory gap не
  скрывать.
- Разбор памяти на PID 50434 показал, что conventional leak почти отсутствует:
  `leaks` нашёл 448 KiB при примерно 768 MiB выделенного heap. Живая модель Name
  Index сама требует не меньше 504.7 MiB: записи 155.1, фильтры 38.8, узлы
  каталогов 49.7, отдельно выделенные имена 195.7, списки/карты/дубли имён не
  меньше 65.5 MiB. После загрузки первый diff-rescan также удваивает capacity
  трёх плоских массивов и оставляет около 243.5 MiB пустого malloc-резерва; он
  влияет на allocated heap, но почти не на физический footprint.
- Memory follow-ups заведены отдельными дочерними тикетами #23: убрать удвоение
  capacity (#35), потоково читать/писать индекс без 280-MiB буферов (#36),
  компактный или mmap-формат Name Index v4 (#37), ограничить WebContent на
  больших Location (#38). После GUI-прогона 50k WebContent имел footprint
  183.5 MiB и peak 279.7 MiB, потому что полный `Item[]` остаётся в Tab state.
- Баг фокуса глобального шортката (#34) закрыт: реализация и физическая
  проверка находятся в запушенном коммите 69d5a6d.
- До закрытия #31 ещё нужны как минимум post-reboot launch, отдельный живой
  проход Spaces/fullscreen, сквозной keystroke ≤8 мс, пятиминутная excursion
  и оставшиеся строки приёмки из SPEC §16. Не закрывать тикет только по этому
  проходу.

## Состояние после оптимизации поиска 24.08

- #31 заклеймлен. На чистом живом v2-индексе (5 040 894 записей) повторяемый
  release-бенч подтвердил регрессию: `процедура приемки` 85 мс,
  wrong-layout `ghjwtlehf ghbtvrb` 79 мс, `kirill macbook` 83 мс,
  заведомый miss 65 мс при бюджете первого результата ≤50 мс.
- Локально реализован компактный 64-битный prefilter имени, SIMD-поиск ASCII
  через `memchr`, более плотный DirId mask-cache и ограничение полного скана
  восемью шардами. На том же живом индексе медианы: 20.1 / 18.8 / 15.7 /
  2.4 мс соответственно. Ранжирование и семантика поиска покрыты тестами.
- Формат Name Index поднят v2→v3: фильтр сохраняется рядом с каждой записью.
  Индекс вырос примерно с 237 до 276 MiB; первый v2→v3 запуск загрузил его за
  710 мс, повторный холодный запуск v3 — за 532 мс (5 043 662 записи), оба в
  бюджете скрытого prewarm ≤2 с.
- `pnpm typecheck`, `pnpm lint`, production frontend build, `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, 70 Rust-тестов и release
  `pnpm tauri build` зелёные. Сборка установлена в `/Applications/Beeline.app`,
  предыдущая копия лежит в `/tmp/beeline-install-backup.VidMf6/Beeline.app`.
- Физический frontend-прогон подтвердил результат. Финальные
  `процедура приемки`, wrong-layout и контрольный `gh` не попали в
  slow-телеметрию, то есть каждый завершился за ≤25 мс. Первый символ `g`
  занял 53 мс, но ядро поиска — только 12 мс. Оставшиеся 41 мс находятся в
  очереди Tauri, IPC или WebView; в ответ и телеметрию добавлен отдельный
  `backend_duration_ms`, чтобы следующий проход измерял эти слои раздельно.
- Текущая сборка с user-initiated QoS поиска стоит в
  `/Applications/Beeline.app`; предыдущая сборка лежит в
  `/tmp/beeline-search-qos-prev.QMxcKp/Beeline.app`. Сам #31 не закрывать: в
  нём ещё есть первый-символ 53 мс и остальные строки performance validation.

## Состояние после дневного захода 24.08

- Перф #31: кэш масок «токен→бит по DirId» вместо планового кэша строк
  (строковый вариант оказался регрессией 2.5–3.5×, замерено до лендинга).
  Смержено в main (68ff043 + мерж 5a43377). Синтетика 5М: холодный
  двухтокенный 48→33 мс. Живой замер утром: 132 мс (было 315) — снят ДО
  дедупа индекса, новую цифру снять после реальной печати Кирилла.
- Найден и починен серьёзный баг (0eb49b6): apply_fs_event на каталог
  реиндексировал поддерево без проверок — индекс распух до 5.89М при 5.0М
  реальных, дубли в выдаче («и опять откуда это» у Кирилла = 15 копий
  одного пути), и убитое переиспользование кандидатов (каждый дубль-add
  бампал revision — гейт mod.rs:307 сбрасывал кэш). Формат индекса v2,
  чистый переобход: 5 040 635 записей за 64 с. Приёмка на живом индексе:
  «процедура приемки» ровно 2 хита (vault + зеркало).
- Тот же коммит: Cmd+T/W/1–9/K/L/[/]/, работают из поискового инпута
  (раньше isEditableTarget глотал все Cmd); junk-сид v2 (Containers, Group
  Containers, Application Support, Logs, Saved Application State) с
  миграцией сохранённых настроек по junk_seed_version.
- ОТКРЫТЫЙ БАГ: шорткат показывает окно БЕЗ клавиатурного фокуса
  (телеметрия: 9 реальных нажатий подряд, focused:false; вставка Кирилла
  улетела в другое приложение). activateIgnoringOtherApps (lib.rs:294) на
  новых macOS кооперативный. Фикс не начат — Кирилл «го» не давал.
- Свежая сборка main (0eb49b6) стоит в /Applications/Beeline.app.

## Состояние после ночного прогона 23–24.08 (история)

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
