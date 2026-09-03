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
Закрытый тикет = сделан и проверен, у закрытого есть комментарий-вердикт.
Открытый с assignee = был в работе; смотри его комментарии и `git log`.

## Search v2 wayfinder начат — 27.08

- Каноническая карта: [Wayfinder Map: Search v2](https://github.com/Kiri110K/beeline/issues/42).
  Цель — полная implementation-ready спека, не реализация.
- Исходное обсуждение записано и закрыто в
  [Set the Search v2 direction](https://github.com/Kiri110K/beeline/issues/43).
  Search v2 сначала ищет в личном Working Set, затем расширяется на Name Index по
  горячо настраиваемому порогу. Search Memory переносит приоритет между похожими
  запросами; fuzzy работает с первого символа. Совместимость с v1 не требуется.
- В `CONTEXT.md` добавлены канонические термины Working Set и Search Memory.
  Простая видимость строки в Search Results не считается сигналом.
- Отложенное Gen2-направление записано в
  [Gen2: explore an always-results interaction model](https://github.com/Kiri110K/beeline/issues/52):
  вместо отдельного Search Results overlay рассматривается одна Raycast-like
  панель, которая всегда показывает ранжированные результаты и даёт действия над
  Item без обязательного Reveal. Роль текущей Location пока не решена. Search v2
  поэтому специфицирует получение, объединение и ранжирование результатов без
  зависимости от Browse Mode / Search Mode и финальной оболочки Gen2.
- AFK-исследование [Research typo candidate retrieval for Name Index v4](https://github.com/Kiri110K/beeline/issues/50)
  завершено на реальном индексе из 5 150 533 Items. Для ограниченной исторической
  части Working Set рекомендован полный Damerau-OSA проход. Общего лимита у
  Working Set нет: текущая Location входит полностью. Для неё и глобального поиска
  архитектура пока не выбрана: нужно сравнить relaxed mmap full-scan baseline,
  FST + automaton для whole-name/token и mixed bigram/trigram postings для fuzzy
  substring. Старый лимит первых 4096 совпадений по mmap order не годится: до
  ранкера должен доходить rank-aware top-k с обязательным union Working Set.
- [Define the Working Set and Search Memory contract](https://github.com/Kiri110K/beeline/issues/45)
  закрыт.
  Для исторических источников принят отдельный общий предел; конкретные значения
  определит прототип. Пассивные visibility, scroll, hover, focus и selection не
  обучают ранжирование. Action Menu даёт слабый сигнал, Quick Look — средний,
  выполненное недеструктивное Item action — сильный. Отдельный
  [Prototype repeated Search Memory signals](https://github.com/Kiri110K/beeline/issues/53),
  закрыт как `not planned`: первая встроенная версия напрямую накапливает события
  без специального объединения повторов. Решение об усложнении принимается после
  реального использования Кириллом по локальным диагностическим логам. Память не
  имеет TTL, плавно стареет и вымывается лимитом; Settings даёт только
  `Reset Learned Ranking`. Удалённый Item и Item на отключённом диске не
  показываются, но их память остаётся неактивной. Серый `No Access` разрешён
  только для Item, который существует, но сейчас недоступен из-за прав.
- [Define query similarity and memory transfer](https://github.com/Kiri110K/beeline/issues/44)
  закрыт. Exact normalized query всегда получает прямую связь с Item. Обычная
  интерпретация переносит память через порядконезависимую Query Family, а Path
  Interpretation сохраняет порядок частей пути и допускает пропущенные каталоги.
  `work wip` и `work/wip` делят путевую память; `~`, `./` и `../` сохраняют
  смысл. Раскладка делится памятью только при фактическом corrected match.
  Префиксы, одна добавленная или удалённая часть и ограниченный edit distance
  переносят память со штрафом. Длина для опечатки считается по изменённой части.
  Переносы не транзитивны, и первая версия не складывает несколько исправлений.
  Результат, пришедший только из памяти, усиливает только точную связь.
- [Prototype the staged retrieval threshold](https://github.com/Kiri110K/beeline/issues/51)
  завершён на production v4 из 5 244 905 Items и двух реальных Working Set
  размером 215 и 321 Item. Дефолт — пять значимых символов включительно.
  Значимыми считаются Unicode-буквы и цифры после NFC; разделители и пунктуация
  не считаются, слова суммируются. Exact existing path и точный Alias Dictionary
  bypass порога. Четырёхзначного исключения для цифр нет: `1825` полезен локально,
  а `2026` и `2035` дают слишком много глобального шума. Изменение порога должно
  немедленно пересчитывать неизменённый активный запрос. Воспроизводимый код и
  отчёт: `/Users/kiri110k/lab/beeline/prototypes/staged-retrieval-threshold/`.
- Продукт делается для одного пользователя и быстрых итераций. Не сохранять
  compatibility с v1 или экспериментальным learned state. Настройки ранжирования
  держать централизованными и дешёвыми для изменения. При замене search/ranking/
  persistence path удалять старую ветку в том же изменении; не оставлять dual
  pipeline, fallback и мёртвый код «на всякий случай».
- [Decide the Search v2 ranking contract](https://github.com/Kiri110K/beeline/issues/49)
  закрыт. Принят единый ranker поверх Candidate Evidence и один активный
  `ranker.json`: строгая атомарная валидация, embedded default при отсутствии или
  ошибке на старте, ручной reload без file watch и немедленный пересчёт активного
  запроса. CLI обязан валидировать, объяснять, replay/diff и применять конфиг;
  `apply` атомарно заменяет файл и просит запущенную Beeline перечитать его. Если
  процесс не подтвердил reload, новый файл остаётся источником правды, а CLI
  явно сообщает, что процесс продолжает работать со старым снимком.
  Ranking Traces локальные: compact на каждое обновление, detailed top-256 после
  300 мс idle или действия, acted-on Item всегда detailed; исходные пределы —
  30 дней или 256 MiB. `schemaVersion` нужен для валидации, а не совместимости:
  экспериментальные конфиг, traces и learned state можно выбрасывать.
  В alpha конфиг полный: только известные коду features, числовые веса, caps,
  penalties и простые curves; язык формул не нужен. Delta относительно default
  отложена до публикации. Каждый effective config хранится один раз по fingerprint,
  пока на него ссылается Ranking Trace. Exact score ties разрешаются по
  нормализованному имени, полному пути и stable Item identity. Открыты semantic
  validation не позволяет конфигу развернуть смысл feature: positive weights,
  penalty magnitudes и caps неотрицательны, curves соблюдают объявленную
  монотонность. Candidate Evidence хранит raw facts; ranker приводит каждый
  применимый факт к силе 0–1 и умножает её на weight. Ranking Trace раздельно
  пишет raw value, normalized strength, weight и contribution. Score — сумма
  ограниченных named groups минус penalties. Связанные features сначала
  складываются внутри group cap; code-defined modifiers вроде query length или
  transfer similarity меняют родительский вклад и отдельно видны в trace.
  Первые groups: Text Match, Search Memory, General Usage, Context, Alias,
  Item Kind и Penalties. Retrieval provenance и Working Set membership дают
  ноль score. Text Match берёт одно лучшее полное explanation: одна
  interpretation, не больше одной correction, лучший hit каждого token и смесь
  weakest/remaining token quality. Search Memory берёт strongest association;
  события внутри неё накапливаются с saturation. General Usage сохраняет
  отдельные contributions под общим cap без cross-source dedup в первой версии.
  Hidden и Junk применяются по одному разу, складываются и ограничиваются общим
  cap. Literal filename exact сильнее near-exact stem; extension — отдельный
  слабый target, который усиливает явно введённая точка. Current Location
  усиливает только непосредственные Items без лимита количества. Отношения между
  groups полностью настраиваются: сценарии первой сборки — гипотезы, не вечные
  гарантии. Числа выбираются при integrated implementation и затем тюнингуются
  по Ranking Traces и использованию Кирилла; отдельный ranker prototype не нужен.
- Следующий открытый frontier —
  [Prototype staged result-stream behavior](https://github.com/Kiri110K/beeline/issues/46).
  Progressive merge сохраняет по stable identity только Focused Item после
  явной result navigation; auto-focused первый Item до навигации не sticky.
  Остальной список свободно rerank; hover и scroll ничего не закрепляют и не
  обучают. Query change сбрасывает сохранение. Stream сообщает local-ready,
  global-running и complete; до 150 мс индикатора нет, после — Status Strip.
  Действующие бюджеты: UI response на keystroke ≤8 мс, first Search Results
  end-to-end ≤50 мс. Отдельный global-completion budget ещё решает #47. Важно:
  threshold prototype замерял нынешний exact/prefix/substring matcher и
  Keyboard Layout Correction, но не Typo Correction; `метолология` после
  полного скана 5 244 905 Items вернула ноль. Его whole-index
  median/p90/max 32.89/63.50/68.76 мс нельзя считать замером полного fuzzy
  Search v2. Отдельно измерить typo, layout+typo, Candidate Evidence, ranking,
  cancellation, IPC и rendered merge.
- В рамках карты production-код не менять. Каждая HITL-сессия использует
  `grilling` и `domain-modeling`; карта хранит указатели, ответы живут в resolution
  comments соответствующих decision tickets.

## Performance pass #31 завершён — 27.08

- В предыдущем проходе закрыты startup/search/Recents/large-list/energy бюджеты:
  persisted v4 fast validation + prewarm дали `index_loaded` 379 мс и usable
  frame 619 мс после reboot; первый keystroke frame — 3 мс; New Temporary Tab —
  43 мс; обычный каталог — 38 мс; Quick Look dispatch — 2 мс; Recents refresh —
  127 мс. Charged physical footprint main process после prewarm/search — 56 MiB.
- Финальный cross-Space smoke нашёл ошибку `CanJoinAllSpaces`: скрытое окно
  оставалось привязано к прежнему Desktop, хотя macOS активировал Beeline.
  Collection behavior заменён на `MoveToActiveSpace | FullScreenAuxiliary`;
  focus retry сокращён с 40 до 20 мс. На втором Desktop show→paint/focus составил
  31/30 мс. Поверх full-screen: 59/21 мс сразу после перехода и 31/23 мс на
  повторном вызове. Окно осталось на вызывающем Space, без переброса к T3.
- Smoke обнаружил, что Focused Item и Selected Items были фактически склеены:
  Escape оставлял hero выбранным, поэтому application Action Menu с New Tab /
  Paste Path был недостижим. Теперь Escape очищает Selected Items, сохраняя
  Focused Item; второй Escape скрывает окно. Application и item menus проверены.
- Контекстное меню вкладки закрывалось глобальным `pointerdown` до выполнения
  `click`, поэтому Pin/Unpin/Remove/Copy Location не работали мышью. Внутренний
  pointerdown теперь не всплывает. Реальный Pin создал `pinned_tabs.json`, Unpin
  вернул Temporary Tab и снова записал пустой список.
- Полный SPEC §16 smoke прошёл на signed test bundle: Paste Path (с системным
  clipboard consent), Reveal, native Quick Look, Copy Path, Open in Terminal в
  точном каталоге, Move to Trash с восстановлением fixture, wrong-layout поиск
  скрытого `.секреттестовый.txt`, Pin → Excursion. После 383 078 мс скрытого
  состояния телеметрия записала `excursions_reset count=1`, а первый листинг был
  Anchor, не `sub`.
- Артефакты и снимки финального прохода лежат в
  `/private/tmp/beeline31-final.OQXBjS`; ключевые события сохранены в
  `state-after/telemetry.ndjson`. Установленная `/Applications/Beeline.app` не
  заменялась. Settings, Recents cache, Visit Journal и clipboard восстановлены
  побайтно; тестовый pin снова отсутствует; Trash-fixture возвращён; Ghostty
  скрыт; пользовательская `.claude/` не тронута.
- Финальная проверка: 93 Rust tests passed, 8 ignored; четыре JS contract tests,
  typecheck, lint, fmt, clippy `-D warnings`, production frontend и подписанный
  Tauri bundle зелёные. #31 и родительский v1 tracker #23 закрыты с verdict;
  оставшиеся #33/#39/#40 относятся к post-v1.

**Следующее:** v1 принят. Не расширять его post-v1 задачами без нового решения;
карта #1 остаётся верхнеуровневым трекером.

## Name Index watcher #41 завершён — 27.08

- Причина многоминутного `remove_slots` найдена в mapped v4: `MappedBase`
  не отсекал `node_id` новых overlay-каталогов. Такой id попадал в секцию
  Item-записей, а четыре байта `name_offset` читались как число прямых детей.
  На реальном индексе это превращало событие первого нового подкаталога в обход
  миллионов чужих слотов.
- `node_record` и `entry_record` теперь проверяют свои счётчики. Добавлены две
  mapped-регрессии: overlay видит только собственных детей, а последовательность
  FSEvents «новый родитель, затем его ребёнок» ничего лишнего не удаляет.
- Batch сортируется от родителя к детям. Потомки отсутствующего пути отбрасываются.
  Если новый каталог уже целиком обошли, его дочерние события того же batch тоже
  отбрасываются. Удаление больше не выделяет и не обнуляет bitset размером со весь
  5-миллионный индекс для каждого маленького поддерева.
- Rename, recreate и смена file↔directory теперь заменяют старый тип полностью.
  Ошибка чтения существующего каталога больше не считается пустым каталогом и не
  удаляет его индексированных детей.
- Телеметрия `index_fs_batch_finished` пишет received/unique/applied paths,
  оба вида pruning, added/removed slots и duration.
- Guarded live pass на persisted `home.idx` прошёл. Создание 11 002 записей дало
  104 коротких batch: 22 124 received, 11 148 unique, 1 645 applied,
  9 499 pruned как уже индексированные, 11 003 added с одним посторонним событием.
  Максимальный batch занял 47 мс, сумма — 302 мс; после drain main был 0.3% CPU.
  Перемещение дерева в Trash добавило и удалило 11 002 слота за 498 мс без
  зависания. Последующее физическое удаление содержимого прошло двумя batch за
  91 и 1 мс.
- Финальный UI pass активировал строго PID test bundle `29723`; один свежий
  sentinel появился под правильным путём, `Searching…` завершился. Скриншот:
  `/private/tmp/beeline41-final.STJWoH/ui-exact-pid/sentinel-present.png`.
  Ранние UI-снимки из того же каталога не использовать: Computer Use тогда поднял
  установленную копию по имени. Этот процесс остановлен перед финальным pass.
- Зелёные: `cargo test` — 92 passed, 7 ignored; fmt, clippy с `-D warnings`,
  `pnpm test`, typecheck и lint. Финальный release bundle после всех правок
  собран и подписан identity `Beeline Dev Signing`.
- Все тестовые PID и фикстуры удалены, clipboard восстановлен. `settings.json` и
  persisted `home.idx` сохранили SHA-256 `8535f09…3d8` и `e75427c…17ab9`.
  `/Applications/Beeline.app` не заменялась. Пользовательская `.claude/` не тронута.
- Реализация и отчёт закоммичены как `b6bba98`, запушены в `main`. В задаче #41
  оставлен verdict с проверками и живыми замерами; задача закрыта.

**Следующее:** вернуться к общему performance pass #31. Bounded persistence
остаётся post-v1 задачей #40.

## WebContent memory #38 завершён — 27.08

- Frontend Location хранит только ограниченное окно строк. Preview ждёт 150 мс
  устойчивого фокуса перед metadata/excerpt/thumbnail IPC, поэтому быстрая
  клавиатурная и wheel-навигация больше не ставит тысячи устаревших запросов в
  очередь WebContent и общего blocking pool.
- Точный существующий путь возвращается до blocking pool и без сканирования
  Name Index. Reveal передаёт явную глобальную позицию прокрутки, поэтому
  восстановление старого scrollTop больше не сбрасывает окно обратно к строке 0.
- Финальный GUI stress-pass на каталоге из 50 000 файлов прошёл. WebContent:
  Recents 39.649 MiB, после полного сценария 67.813 MiB, после idle 66.255 MiB;
  рост 28.164/26.606 MiB при лимите 64 MiB, абсолютный максимум ниже 120 MiB.
  Первый кадр большого каталога занял 47 мс.
- Непрерывность проверена на строках 250, 750, 1250, 1750, 2250 и 2500.
  Пустых строк, перестановки и потери фокуса нет. Точный путь
  `file_30000.txt` открылся без состояния Searching; Reveal вернул окно с
  offset 29968 и нужной строкой; Quick Look перешёл на `file_30002.txt`.
- Автопроверки зелёные: 87 Rust-тестов прошли, 6 ignored; `cargo fmt`, clippy
  с `-D warnings`, `pnpm typecheck`, lint и весь набор JS contract tests.
  Финальный подписанный `pnpm tauri build` установлен в
  `/Applications/Beeline.app`.
- Отчёты: memory/scroll
  `/private/tmp/codex-computer-use.beeline38-coalesced.fLjJQA/report.md`,
  Reveal/Quick Look
  `/private/tmp/codex-computer-use.beeline38-final-reveal.aD11ne/report.md`.
- Во время stress-pass main process держал 47–63% CPU. `sample` показал все
  2130 выборок в Name Index watcher:
  `apply_fs_event → reconcile_dir_children → remove_child → remove_slots`.
  Это отдельная проблема обслуживания Name Index, не WebContent; она вынесена
  в задачу #41.

**Следующее:** ограничить стоимость удаления больших поддеревьев из overlay по
FSEvents в #41. После этого вернуться к общему performance pass #31. Bounded
incremental persistence Name Index остаётся в post-v1 задаче #40.

## Пауза на WebContent memory #38 — 25.08, перед обновлением T3 Code

### Зачем

Ограничить память процесса WebContent на Location с десятками тысяч файлов.
Старый путь держал полный `Item[]` каждой вкладки и на каталоге из 50 000
файлов доходил до footprint 183.5 MiB и peak 279.7 MiB. Цель тикета #38:
footprint не выше 120 MiB, рост относительно Recents не выше 64 MiB, первый
кадр не дольше 150 мс, без поломки скролла, Reveal, Quick Look и восстановления
вкладки.

### Состояние кода

- Работа стоит на паузе в незакоммиченном дереве `main` на HEAD
  `71763ebfcc60954e269109e74c6acc49367d965e`. Ничего не пушилось.
- Изменены `package.json`, `src-tauri/src/lib.rs`,
  `src-tauri/src/listing.rs`, `src/App.tsx`, `src/browse/state.ts`,
  `src/components/FileTable.tsx`, `src/location/ipc.ts`,
  `src/location/schema.ts`, `src/preview/model.ts`,
  `src/settings/SettingsView.tsx`, `src/tabs/useTabs.ts` и этот хэндоф.
  Добавлен `scripts/browse-window.test.ts`. Пользовательская `.claude/`
  была untracked до начала работы и не тронута.
- Backend теперь создаёт ограниченные по жизни listing sessions с владельцем
  Tab и поколением запроса. Начальный ответ содержит 64 строки метаданных,
  общее число строк, `sessionId`, смещение и разрешённые позиции focus/selection.
- Frontend держит окно до 2048 строк вместо полного списка. Таблица сохраняет
  полную виртуальную высоту и просит следующее окно с запасом 512 строк.
- Reveal передаёт путь backend, чтобы получить глобальную позицию. Выделение
  диапазона разрешает пути через backend. Quick Look использует локальное окно
  и просит соседа только на его границе; нативной панели передаётся один путь.
- Повторный листинг той же вкладки инвалидирует старую сессию. Просроченная
  сессия автоматически приводит к новому листингу.

### Что уже проверено

- Зелёные: `pnpm typecheck`, `pnpm lint`, `pnpm test`, `pnpm build`,
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` и `git diff --check`. В Rust: 83 passed, 6 ignored, 0 failed.
- Подписанный release-бандл собран в
  `/Users/kiri110k/lab/beeline/src-tauri/target/release/bundle/macos/Beeline.app`.
  `/Applications/Beeline.app` не заменялся.
- GUI-прогон использовал каталог
  `/private/tmp/beeline-50k-window.xnlpTt` с ровно 50 000 пустых файлов.
  Все артефакты лежат в
  `/private/tmp/codex-computer-use.beeline38.Up75tR`.
- На Recents WebContent имел footprint 57 MB и peak 80 MB. На большом каталоге
  четыре последовательных замера дали 70, 73, 57 и 102 MB. Максимальный
  текущий footprint укладывается в 120 MB, а максимальный рост относительно
  Recents равен 45 MB и укладывается в 64 MB. Peak процесса был 169 MB, но peak
  не входит в сформулированный порог #38. Сырые данные:
  `/private/tmp/codex-computer-use.beeline38.Up75tR/memory-recents-baseline-raw.txt`
  и
  `/private/tmp/codex-computer-use.beeline38.Up75tR/memory-large-location-samples-raw.txt`.
- Большой каталог отрисовался в отсортированном порядке. Начальный backend
  listing вернул 64 из 50 000 строк за 332 мс. Это выше бюджета первого кадра
  150 мс, но событие `location_first_frame` в телеметрию не попало, поэтому
  сквозное время пока не измерено. Артефакт:
  `/private/tmp/codex-computer-use.beeline38.Up75tR/large-tab-first-frame-telemetry.txt`.
- Автоматизация отправила 460 ArrowDown и затем ещё 70 ArrowDown. Интерфейс
  оставался отзывчивым, но после прокрутки появилась возможная потеря фокуса
  или несогласованность строки. Wheel-драйвер также несколько раз не сдвинул
  список. Это может быть ограничением Computer Use, а может быть ошибкой окна;
  вердикта нет. Снимки и заметки находятся в каталоге артефактов.

### Cleanup перед паузой

- Исходный файл настроек восстановлен из
  `/private/tmp/codex-computer-use.beeline38.Up75tR/settings.json.original`.
  SHA-256 восстановленного файла:
  `8535f09a97f1a134808a6eeb17fbae9def71fc7ebf87f95da9e13e25b8cfe3d8`.
- Тестовая сборка с PID 73391 и её WebContent с PID 73412 остановлены.
- Временный каталог 50 000 файлов оставлен для продолжения. Если `/private/tmp`
  очистится после перезагрузки, создать его заново с теми же именами
  `file_00000.txt`…`file_49999.txt`.

### Осталось

- Разобраться, почему сортировка 50 000 имён и первые 64 metadata занимают
  332 мс, и вернуть сквозное измерение `location_first_frame`.
- Повторить холодный GUI-прогон и получить устойчивый первый кадр не дольше
  150 мс.
- Проверить непрерывный скролл через несколько границ окна без Computer Use
  bulk-key ambiguity. Отдельно проверить Shift+Click, Reveal, Quick Look через
  границу окна и восстановление вкладки.
- Провести ревью незакоммиченного diff. После зелёного GUI-вердикта сделать
  коммит, push, комментарий с замерами и закрыть #38.

**Первый следующий шаг:** открыть снимки
`/private/tmp/codex-computer-use.beeline38.Up75tR/after-460-inputs-state.jpg` и
`/private/tmp/codex-computer-use.beeline38.Up75tR/large-list-after-row-450.jpg`,
сверить ожидаемую строку с текущим окном и локализовать возможную ошибку
прокрутки до нового GUI-прогона.

## Возобновление после reboot: Name Index memory runaway — 27.08

- Инцидент после перезагрузки был не артефактом незаконченного WebContent WIP.
  Установленная сборка отличалась от тестового bundle. Два системных
  `cpu_resource` report показали main-process Beeline: 7.7–14.8 ГБ footprint,
  85–89% CPU, горячий стек `diff_rescan → diff_rescan_dir → child_dirs/crawl`.
- Реальный v4-файл: 5 150 533 Items, 418 194 directory nodes, 309 860 406 байт.
  Структура и directory links валидны; нового содержимого на диске было лишь
  около 20 тысяч записей. Причина была алгоритмической, не объёмом изменений.
- Устранены три runaway-пути: свежие широкие поддеревья больше не делают
  квадратичный sibling lookup; удаление поддерева теперь итеративное с компактным
  bitset (реальный recursive path падал после 10 102 `remove_slot/clear_children`
  frames); startup traversal идёт по компактным directory-node links, а не
  перечитывает миллионы file Items. Сам diff-rescan стал итеративным с `visited`.
- Mapped v4 теперь валидирует однозначную связь directory Item ↔ node и хранит
  tombstones узлов. Добавлены регрессии на 2 000 широких siblings, удаление
  20 000 уровней без call stack и удаление mapped directory tree с повторным save.
- Полная startup-compaction существующей v4-базы сознательно отложена: старый
  mmap и растущий temp-файл дают большой bounded transient даже после потоковой
  оптимизации. После diff актуальный overlay остаётся authoritative для сессии;
  следующий запуск повторяет тот же быстрый diff. Первый crawl по-прежнему пишет
  v4. Телеметрия: `index_persist_deferred`; нужен отдельный bounded incremental
  compaction follow-up, после чего startup persistence можно вернуть.
- Два финальных запуска свежего release-бинарника на реальном индексе прошли под
  fail-closed watchdog, который каждую секунду читает суммарный
  `ri_phys_footprint` main + новых WebContent и останавливает на 512 МиБ:
  diff-rescan 940/476 мс, 23 655/23 654 mutations, максимум process tree около
  100 МБ; после 25 секунд idle стабильно около 81 МБ. Накопления на повторном
  запуске нет. Артефакты:
  `/private/tmp/beeline-reboot-fixed23.ymC43C` и
  `/private/tmp/beeline-reboot-repeat.6Bg5GQ`.
- Реальные `home.idx` и `settings.json` не заменялись. Их SHA-256 остались
  `e75427c112786f23fae4d6140dc86619bfc7f315c8d73b3a1b6fecaec4317ab9` и
  `8535f09a97f1a134808a6eeb17fbae9def71fc7ebf87f95da9e13e25b8cfe3d8`.
  Все тестовые PID остановлены; `/Applications/Beeline.app` не заменялся.
- Зелёные: `pnpm typecheck`, `pnpm lint`, `pnpm test`, `pnpm build`,
  `cargo test` (86 passed, 6 ignored), `cargo clippy --all-targets -- -D warnings`,
  `cargo fmt --check`, `git diff --check`. Release code/bundle собраны, но после
  reboot в Keychain нет identity `Beeline Dev Signing`, поэтому финальный
  `tauri build` закономерно падает только на codesign. Для runtime-проверок
  использовался свежий ad-hoc release binary; установленная копия не тронута.

**Следующее:** оформить GitHub follow-up на bounded incremental compaction,
закоммитить текущий общий WIP без `.claude/`, затем вернуться к оставшейся
WebContent/UI-приёмке #38.

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
  установлен в `/Applications/Beeline.app`; предыдущая установленная сборка
  сохранена в `/private/tmp/beeline-name-index-v4-install-backup.Cip6Xs/Beeline.app`.
  Текущий реальный `home.idx` уже v4; предыдущий v4 сохранён в
  `/private/tmp/beeline-v4-pre-fix-backup.XTOjOr/home.idx`, исходный v3 — в
  `/private/tmp/beeline-v3-backup.zRJKFP/home.idx`.
- Реализация, прототип и замеры закоммичены как `767f4e1` и запушены в `main`.
  Пользовательская `.claude/` остаётся нетронутой и untracked.

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
