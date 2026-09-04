# HANDOFF — реализация Beeline alpha

Обновляется после каждого закрытого куска работы. Читатель — новая сессия
Claude Code на этом Маке, без доступа к прошлой.

## Задача словами Кирилла

Ночной goal от 05.09: без участия Кирилла закончить определённые части Search v2
и performance/energy fixes, проверять установленное приложение и сохранять
checkpoints в git/GitHub. Когда основной backlog закончится, исследовать в
отдельных ветках альтернативы производительности и энергопотребления. Не брать
отложенные продуктовые решения, которым нужен Кирилл.

## Источник правды по прогрессу

GitHub-трекер: https://github.com/Kiri110K/beeline/issues/23 (родитель).
Закрытый тикет = сделан и проверен, у закрытого есть комментарий-вердикт.
Открытый с assignee = был в работе; смотри его комментарии и `git log`.

## Search v2 wayfinder начат — 27.08

- Карта решений завершена 05.09. Канонический контракт перенесён в §6, §10–12,
  §14 и §16–17 `docs/SPEC.md`; термины Stable Item Identity и Ranked Result
  Stream добавлены в `CONTEXT.md`. Search v2 теперь специфицирован независимо от
  нынешнего overlay и будущей Gen2-панели.
- Финальные alpha-бюджеты: first useful end-to-end p95 ≤50 мс, UI response ≤8 мс,
  warm complete top-50 p95 ≤500 мс, hidden settled footprint <200 MiB, startup
  peak <400 MiB, persisted q-gram sidecar ≤600 MiB. Learned state по умолчанию
  ограничен 64 MiB; Ranking Traces — 30 дней или 256 MiB. Cold/rebuild остаётся
  progressive и cancellable, но не задерживает полный Working Set.
- Следующая реализация без нового продуктового решения: Search Memory и learned
  usage, затем единый configurable ranker, `ranker.json`, Ranking Traces, CLI,
  manual reload и Reset Learned Ranking. Старый ranking/persistence path удалять
  в том же изменении; compatibility с экспериментальными данными не сохранять.
- Первый Search Memory срез реализован 05.09 в #55. Append-only NDJSON хранит
  query-independent usage и отдельные ordinary/path associations; слабый Action
  Menu, средний Quick Look и сильные Reveal/open/enter сигналы подключены к UI.
  Нормализация, unordered Query Family, ordered `work wip` / `work/wip`,
  prefix, single-token add/remove и bounded edit transfer реализованы без
  транзитивного сложения. Learned paths входят в Working Set и могут вернуться
  как memory-only result; missing paths остаются dormant. Есть continuous aging,
  saturation, 64 MiB log compaction, weakest-first association limit и двухшаговый
  Reset Learned Ranking в Settings. Полный suite: 117 Rust passed / 9 ignored,
  clippy `-D warnings`, frontend contracts, lint, typecheck и build зелёные.
- Единый `ranker.json` срез завершён следом. Все production score bands, Search
  Memory и usage caps, Visit/Recents, current Location/Pinned/Known Place, Alias,
  Item Kind, Hidden/Junk и global threshold читаются из одного strict config.
  Missing config использует embedded default; unknown/missing/negative/non-monotone
  значения отвергают файл целиком. Manual reload атомарно меняет snapshot, отменяет
  старую backend wave и перезапускает неизменённый активный запрос. Settings имеет
  reload control; CLI умеет default, validate/explain, compare и atomic apply с
  честным `reloadConfirmed: false`, если running app не подтверждён. Дефолтный
  конфиг сохраняет прежний порядок; отдельный тест доказывает изменение порядка
  одним weight без изменения retrieval. Зелёные: 120 Rust passed / 9 ignored,
  clippy `-D warnings`, frontend contracts, lint, typecheck и production build.
- Learned identity теперь переносится при Rename внутри Beeline: rebind меняет
  сам Item и все retained descendant paths, сливает уже существующую статистику
  нового пути и воспроизводится из NDJSON после restart. Copy File/Path, Open in
  Terminal/Editor, Reveal in Finder и Open in New Tab также дают strong signal
  только после успешного dispatch; Trash/Delete/failed actions не обучают.
- Ranking Traces получили первый production writer: один background-QoS worker
  без polling пишет compact record каждого query update, отменяемый detailed
  top-256 через 300 мс idle и немедленный action record. Каждый record содержит
  config fingerprint; effective configs сохраняются content-addressed в
  `ranker_snapshots/`. NDJSON чистится по 30 дням и 256 MiB, maintenance идёт
  только на startup/новом событии. UI по-прежнему получает top-50; расширенный
  top-256 остаётся локально для анализа.
- Detailed trace теперь хранит для каждого top-256 Item точный final score и
  named contributions: Text Match, Search Memory, General Usage, Context, Alias,
  Item Kind и Penalties. Инвариант суммы покрыт тестами для literal, fuzzy,
  memory-only и hidden результатов. Memory-only Hidden/Junk теперь корректно
  получает penalty. CLI `replay` строго парсит NDJSON и проверяет contiguous rank,
  невозрастающий score и совпадение score с contribution total. Config snapshots,
  на которые больше не ссылаются retained traces, удаляются во время maintenance.
- Trace schema v2 теперь сохраняет config-independent Candidate Evidence и для
  каждого применимого leaf feature отдельно пишет raw value, normalized milli,
  weight и signed contribution. Text Match разложен на exact/prefix/substring/
  typo/path/existing-path, typo edit count, all-tokens bonus, path scope и layout
  correction; остальные facts покрывают learned association/usage, visits,
  Recents, Current Location, Pinned Anchor, Known Place, Alias, Item Kind и
  Hidden/Junk. Replay заново собирает contributions из fingerprinted config и
  отвергает несовпадение. `replay <trace> <alternative-config>` пересчитывает
  весь top-256 без Name Index или filesystem, сообщает изменившиеся snapshots и
  до 1000 точных old/new rank+score deltas. Старые trace schema намеренно не
  поддерживаются.
- `ranker-config apply` после атомарной записи теперь обращается к запущенной
  Beeline через локальный Unix socket с mode 0600. Один блокирующий background-QoS
  listener без polling загружает strict config, меняет active snapshot, отменяет
  старую search wave, очищает reuse, пишет telemetry/config snapshot и отправляет
  frontend событие для rerank. CLI сверяет подтверждённый fingerprint; если app
  не запущена или ответ не совпал, файл всё равно остаётся источником правды для
  следующего запуска, а `reloadConfirmed` честно остаётся false.
- Живой restart нашёл цикл: startup намеренно держал малую diff-rescan дельту в
  overlay, но graceful exit всё равно переписывал 5.6M-Item base. Уже построенный
  q-gram оставался привязан к предыдущему base hash, поэтому следующий запуск
  снова тратил около 19–21 с CPU и записывал ~554 MB. Shutdown rewrite удалён как
  противоречащий deferred-persistence пути. Initial crawl по-прежнему сохраняет
  base и затем строит sidecar; последующие session deltas восстанавливаются
  быстрым diff-rescan до появления bounded incremental compaction.
- Signed bundle с live control и trace v2 установлен в `/Applications/Beeline.app`.
  UI-pass через настоящий global shortcut прошёл: `метолология` показал
  `МЕТОДОЛОГИЯ.md` rank 2, `ьуерщвщдщпн` дал methodology results с rank 1,
  `work wip` дал `/Users/kiri110k/work/wip` rank 1, финальное окно скрыто.
  Свежий trace содержал 3 result snapshots / 381 ranked Items; установленный CLI
  replay подтвердил его без расхождений и missing snapshots. Live apply вернул
  `reloadConfirmed: true` и точный active fingerprint. После одноразового rebuild
  sidecar занял 554,315,648 bytes; следующий restart загрузил его без helper,
  search prewarm занял 16 ms, diff-rescan 385 ms, hidden footprint 51 MiB.
  Backup до exit-fix: `/private/tmp/Beeline-before-qgram-exit-fix-20260905.app`.
- Bounded overlay persistence (#40) получил append-only path journal. Watcher
  сначала flush-ит relative paths, затем меняет in-memory overlay; partial и
  obsolete alpha records при replay игнорируются. Startup поверх неизменного
  mmap base re-stat/replay-ит уникальные пути, обычный diff-rescan закрывает окно
  crash/missed event. Journal сохраняется между запусками до нового полного base
  snapshot, поэтому уже известная delta не превращается обратно в RAM-only state.
  Повторный путь пишется только один раз; in-memory set
  ограничен 100,000 путей, а при 64 MiB файл переписывается через temp+fsync+
  rename. Тест crash-replay покрывает add, remove, rename и file→directory с
  descendant; полный checkpoint — 132 Rust passed / 9 ignored и clippy green.
- Два последующих live restart остались функционально корректны, но diff-rescan
  занял 3.42/3.85 s и не прошёл `<2 s` acceptance #40. Найдена причина повторной
  работы: file-level FSEvent обновлял Item, но не mtime его parent directory.
  `apply_fs_event` теперь сохраняет parent mtime и для обычных, и для Junk events;
  startup telemetry пишет visited/reconciled/missing/Junk directory counts. Это
  покрыто отдельным тестом; checkpoint — 133 Rust passed / 9 ignored.
- Новая telemetry показала 128,759 visited directories при всего 25 reconciled:
  основное время уходило в последовательные `stat` и повторную сборку full path.
  Diff теперь один раз под read lock строит `(DirId, path, stored mtime)` snapshot
  прямым parent→child обходом, четырьмя bounded utility workers проверяет metadata,
  затем parent-first применяет только реально changed directories с повторной
  проверкой identity. Watcher gate всё это время закрыт, поэтому snapshot стабилен.
- Первый live-pass этой версии дал 4.97 s: workers ошибочно получили background
  QoS, который на macOS throttles metadata IO (противоречило уже записанному
  правилу самого crawl module). Workers переключены на utility QoS. Telemetry
  дополнена отдельными `snapshot_ms`, `metadata_ms`, `apply_ms` для следующего
  измерения; total остаётся источником acceptance.
- Headless release matrix на production base: 1/2/4/8/12 workers дали первый
  проход 400/139/93/82/82 ms metadata; повторные 8/4/2/1 — 94/86/122/205 ms.
  Значит 4 workers достаточно, а многосекундный GUI tail вызван process App Nap,
  не алгоритмом или шириной. `diff_rescan` теперь держит ровно на время проверки
  `NSProcessInfo` activity `UserInitiatedAllowingIdleSystemSleep`: она снимает Nap,
  но не запрещает system sleep, и завершается RAII-drop после diff.
- #40 закрыта коммитом `0a8e27f`. Обычный startup больше не делает 128k `stat`:
  после регистрации live watcher нативный FSEvents cursor возвращает изменения
  с прошлого durable checkpoint, они fsync-ятся в bounded journal и replay-ятся.
  Dropped/wrapped history, отсутствие cursor и journal saturation fail closed к
  full diff с атомарной base compaction; compaction начинается при 80k путей до
  hard limit 100k. Первый migration-pass: diff 1819 мс, compaction 2220 мс,
  peak main/WebContent около 81/36 MiB. Два обычных guarded restart: catch-up
  74/15 мс, replay 19/32 мс, full diff пропущен, process-tree peak 93/94 MiB.
  Переписанная production base структурно загрузилась за 216 мс; q-gram на обоих
  рестартах открылся без helper. Signed `0a8e27f` стоит в
  `/Applications/Beeline.app`, PID последнего скрытого запуска — 58924. Backup:
  `/private/tmp/Beeline-before-fsevents-20260905.app`. GitHub verdict:
  https://github.com/Kiri110K/beeline/issues/40#issuecomment-5544038900
- PR #56 влит в main как `b4eda4c`. Q-gram retrieval больше не создаёт per-token
  `HashMap` и общий `BTreeSet`: один переиспользуемый dense counter очищает только
  touched slots, а плоский кандидатный список сортируется один раз. Same-seed
  2000-query A/B: 127.62 → 101.32 с, candidate retrieval p95 быстрее в 3.8–7.8x,
  peak physical 91.44 → 90.90 MiB, все final hits/fingerprints совпали. Повторные
  1000 queries с другим seed дали retrieval p95 0.055–4.625 мс и peak 67.95 MiB.
  Signed build стоит в `/Applications/Beeline.app`; UI ranks: `метолология` 2,
  `ьуерщвщдпн` 1, `work wip` 1. JSON-отчёты:
  `/private/tmp/beeline-qgram-dense-baseline-20260905.json`,
  `/private/tmp/beeline-qgram-dense-candidate-20260905.json` и
  `/private/tmp/beeline-qgram-dense-repeat-20260905.json`; рядом лежат `.time`.
  GUI report: `/private/tmp/beeline-qgram-dense-ui.uasaJz/report.md`. Его FAIL по
  Escape был ошибкой проверки: SPEC §5 требует оставить query видимым после
  закрытия Search Results, а второй настоящий Escape скрыл окно. Ошибочный #57
  закрыт с коррекцией: https://github.com/Kiri110K/beeline/issues/57#issuecomment-5544278704
- PR #58 влит в main как `cf7d11e`. Внутренние count/offset/write-position
  таблицы q-gram builder переведены с `u64` на checked `u32`; существующий
  sidecar-формат с 64-bit offsets не менялся. Полный production build дал
  побитно одинаковый 554,313,828-byte sidecar. Время: 18.21 → 17.57 с; peak
  physical: 543.88 → 536.09 MiB. Главный остаток — raw postings vector около
  520 MiB.
- Release-profile эксперимент `experiment/release-profile` отклонён и оставлен
  отдельной remote-веткой. Thin LTO + один codegen unit + abort + strip уменьшили
  backend binary на 49.3%, но suite стал на 1.5% медленнее, cycles выросли на
  4.9%, а память не изменилась. В main попал только отрицательный результат
  `prototypes/release-profile/RESULTS.md` (`6d1573b`).
- PR #59 влит в main как `8392f0e`. Multi-token fuzzy scoring теперь один раз на
  кандидата строит lowercase ancestor chain и переиспользует её для ordinary,
  Path Interpretation и corrected-layout вариантов. Same-seed suite: 51.60 →
  44.07 с, то есть 1.17x и на 14.6% меньше wall; user CPU -16.5%, instructions
  -12.0%, cycles -16.4%. Implicit/gapped/direct-multi p95 улучшились на
  16.1%/23.9%/21.8%. Повтор с другим seed: 44.12 с. Hits/fingerprints совпали,
  misses нет; 134 Rust passed / 11 ignored, fmt и clippy зелёные. Установленное
  приложение пока содержит предыдущий dense-qgram build без этого CPU-фикса и
  без builder-only `u32` изменения.
- PR #60 влит в main как `d1a5259`. Fuzzy verifier теперь один раз lowercases
  имя кандидата в существующий worker buffer и переиспользует его во всех
  интерпретациях; уже lowercase ancestor components больше не копируются и не
  нормализуются повторно при каждом сравнении. Относительно PR #59 suite: 44.07
  → 35.70 с, то есть ещё 1.23x и -19.0% wall; user CPU -21.5%, instructions
  -19.9%, cycles -21.5%. Все десять p95 улучшились; path-heavy на 19.0–25.4%.
  Repeat: 35.50 с. Hits/fingerprints совпали, misses нет; 134 Rust passed / 11
  ignored, fmt и clippy зелёные. Артефакты лежат в `/private/tmp` под stems
  `beeline-fuzzy-lowercase-reuse-20260905` и
  `beeline-fuzzy-lowercase-reuse-repeat-20260905` (`.json` + `.time`).
- PR #61 влит в main как `3c209b7`. Каждый fuzzy verifier worker теперь
  переиспользует DirId-вектор и String-capacity ancestor chain между кандидатами.
  Same-seed suite: 35.70 → 33.20 с (-7.0% wall, -10.4% user CPU, -7.8%
  instructions, -10.3% cycles); paired-seed: 35.50 → 33.60 с. Path-heavy p95
  улучшился на 3.8–9.4% в обоих прогонах. Hits/fingerprints совпали, misses нет;
  134 Rust passed / 11 ignored, fmt и clippy зелёные. Артефакты в `/private/tmp`
  под stems `beeline-fuzzy-ancestor-scratch-20260905` и
  `beeline-fuzzy-ancestor-scratch-repeat-20260905` (`.json` + `.time`).
- PR #62 влит в main как `8c45b2e`. Sorted q-gram candidates часто идут
  sibling-группами, поэтому worker запоминает prepared parent `DirId` и повторно
  отдаёт ту же lowercase ancestor slice для следующего sibling. Same-seed suite:
  33.20 → 30.60 с (-7.8% wall, -10.6% user CPU, -6.7% instructions, -10.5%
  cycles); paired-seed: 33.60 → 31.30 с. Path-heavy p95 улучшился на 2.7–10.1%.
  Hits/fingerprints совпали, misses нет; 134 Rust passed / 11 ignored, fmt и
  clippy зелёные. Артефакты в `/private/tmp` под stems
  `beeline-fuzzy-parent-ancestor-cache-20260905` и
  `beeline-fuzzy-parent-ancestor-cache-repeat-20260905` (`.json` + `.time`).
- `experiment/fuzzy-name-quality-reuse` (`2962245`) отклонён и не вливался.
  Кэш `Option<MatchQuality>` для переиспользования между ordinary multi-token и
  Path Interpretation дал paired suites 30.60 → 31.20 с и 31.30 → 30.60 с, при
  одинаковом -0.16% retired instructions. p95 смешанные, есть регрессии; запись
  вектора заменяет всего одно повторное сравнение final token и не уменьшает
  реальную работу. Код, verdict и raw-artifact stems сохранены в remote-ветке.
- PR #63 влит в main как `ca125b2`. Для slash-shaped query verifier сначала
  проверяет final path segment против Item name, и только после успеха строит
  ancestor chain; готовое качество передаётся в ordered path matcher. На 500
  samples `work/wip`: total p95 16.26 → 11.65 мс (-28.3%), verify/rank 15.03 →
  10.42 мс, user CPU -28.7%, instructions -23.2%, cycles -29.0%. Repeat дал
  11.75/10.48 мс. Hits/fingerprint совпали, misses нет. `explicit-path` добавлен
  в стандартный `queries.tsv`; новый 11-case suite занял 32.5 с и нашёл все
  targets. 134 Rust passed / 11 ignored, fmt и clippy зелёные. Артефакты в
  `/private/tmp` имеют prefix `beeline-path-final-first-`; это baseline,
  candidate, candidate-repeat и standard JSON/time pairs.
- PR #64 влит в main как `836e6a4`. Для ordinary multi-token verifier теперь
  сначала доказывает обязательный Item-name match и сохраняет его качество;
  false-positive candidate уходит до ancestor work. Implicit Path Interpretation
  тоже проверяет final name первым. Расширенный 11-case suite: 32.50 → 19.80 с
  (-39.1% wall, -47.1% user CPU, -52.5% instructions, -47.1% cycles). Final p95:
  `work wip` -54.1%, `vault methodology` -37.6%, `status report` -55.7%. Repeat:
  19.40 с с теми же hits/fingerprints и zero misses. 134 Rust passed / 11
  ignored, fmt и clippy зелёные. Артефакты в `/private/tmp` под stems
  `beeline-fuzzy-multi-name-first-20260905` и
  `beeline-fuzzy-multi-name-first-repeat-20260905` (`.json` + `.time`).
- После PR #63 был установлен signed bundle с code commit `ca125b2`: startup
  FSEvents catch-up 20,438 paths за 599 мс, overlay replay 514 мс, full diff
  skipped; после оседания 37 MiB physical footprint, 0 окон, process не
  frontmost. PID 11837. Backup предыдущего dense-qgram bundle:
  `/private/tmp/Beeline-before-search-verifier-20260905.app`. Этот установленный
  bundle ещё не содержит PR #64; его надо заменить следующей сборкой main.
- Signed bundle с PR #64 затем установлен в `/Applications/Beeline.app`, PID
  15111, hidden/0 windows, SHA совпал с build artifact. Startup FSEvents catch-up:
  3,263 paths / 139 мс; replay 16,635 applied paths / 734 мс; full diff skipped.
  Через quiet window обнаружен новый energy-хвост: 499 dirty Junk dirs drained за
  15,047 мс на external power. Backup предыдущего ночного bundle:
  `/private/tmp/Beeline-before-multi-name-first-20260905.app`.
- Независимый GUI-pass установленного PR #64 через настоящий global shortcut
  подтвердил четыре последовательных запроса без restart: `work wip` и
  `work/wip` дали `/Users/kiri110k/work/wip` rank 1, `vault methodology` и
  `status report` полностью заменили предыдущие выдачи без stale rows или
  видимых дублей. Freeze, crash и потеря focus не обнаружены. Два Escape сначала
  закрыли результаты с сохранением query, затем скрыли приложение; PID 15111
  остался жив, hidden/0 windows/not frontmost. Скриншоты и отчёт:
  `/private/tmp/beeline-installed-ui.9YN3vx/`.
- `experiment/junk-single-child-snapshot` (`140fe5d`) отклонён. Удаление второго
  `direct_children` прохода не изменило широкий `target/debug/deps` reconcile:
  baseline 160–172 мс, candidate 151–171 мс, средняя разница <2%. Главная цена —
  перечисление всех ~90k siblings. Код и verdict сохранены в remote-ветке; main
  должен пробовать deferred exact-path apply вместо parent-directory rescan.
- `experiment/junk-exact-path-drain` (`aeb573f`) тоже отклонён. Один existing
  file в 90k-child dir действительно стал 0.023–0.058 мс вместо 160–172 мс, но
  полный production journal не подтвердил выигрыш: directory mode (517 dirs)
  занял 1188/616 мс, hybrid (≈18.3k exact + 5 dirs) 1138/1130 мс. Оба режима
  имели одинаковые 73 disk/index mismatch по явным journal paths, но total Item
  count различался примерно на 3200: parent reconcile дополнительно чинит
  unlisted siblings, exact-path вариант это теряет. 15,047 мс в installed app
  объясняются background-QoS throttling; drain запускается только после quiet на
  AC или синхронно при Junk-targeting query. Текущий main оставлен без изменений.
- PR #65 влит в main как `e9bcfca`. Q-gram builder больше не держит глобальный
  raw postings vector: второй index pass пишет 6-byte `(local bucket, slot)` в 64
  временных shard, затем каждый shard counting-sort-ится и последовательно
  дописывается в прежний sidecar format. Production output побитно идентичен:
  554,313,828 bytes, SHA-256 `33160717b15d8de5a5b8e63d818e38f2b88380cdb350874f5933c0ee5a792107`.
  Peak physical 536.09 → 79.83 MiB (-85.1%), max RSS 864.89 → 412.88 MiB,
  wall 18.55 → 18.41 с; repeat 17.06 с / 79.92 MiB. Цена — 780.95 MiB temp
  records только во время rebuild; RAII cleanup работает на success/error. Три
  одноразовых 529 MiB sidecar удалены после SHA-сверки, `.time`/stdout сохранены
  в `/private/tmp` под prefix `beeline-qgram-sharded-`. Полный checkpoint: 134
  Rust passed / 11 ignored, fmt и clippy зелёные.
- PR #66 влит в main как `be5c803`. Формат `BLQGM002` delta-кодирует sorted
  posting slots unsigned varint и добавляет checkpoint каждые 64 записи для
  bounded literal lookup. Production sidecar уменьшился 554,313,828 →
  194,351,859 bytes (-64.9%). Независимый streaming verifier подтвердил все
  136,481,293 postings и все checkpoints во всех 1,048,576 bucket. Две парные
  11-case A/B серии дали 19.55 → 20.01 с и 19.81 → 19.79 с; zero misses,
  одинаковые top-10 fingerprints и posting visits. Retrieval p95 подорожал лишь
  на 0.03–0.17 мс. Rebuild: 19.37 с, 95.94 MiB physical peak; временные shards
  очищены. Полный checkpoint: 135 Rust passed / 11 ignored, fmt, clippy,
  typecheck, lint и frontend contracts зелёные. Подробности и artifacts:
  `prototypes/qgram-delta-varint/RESULTS.md`.
- Signed bundle `be5c803` установлен в `/Applications/Beeline.app` вместе с уже
  проверенным v2 sidecar, SHA build/install совпадают. PID 53884 запущен hidden,
  0 windows/not frontmost; sidecar загрузился без rebuild. Первый cold prewarm
  занял 171 мс, FSEvents catch-up 193 мс, overlay replay 1,165 мс, full diff
  skipped. Через 19 секунд process physical footprint 42 MiB, peak 74 MiB.
  Rollback: `/private/tmp/Beeline-before-qgram-varint-20260905.app` и
  `/private/tmp/beeline-home-qgram-v1-20260905.qgram`.
- Независимый UI-pass установленного v2 sidecar прошёл в одной живой сессии:
  `ьуерщвщдщпн` дал methodology.md rank 1, `work wip` и `work/wip` дали
  `/Users/kiri110k/work/wip` rank 1. Между запросами нет stale rows, пустой
  выдачи или дублированных exact paths; freeze/crash/focus loss не было. После
  Escape PID 53884 остался жив, hidden/0 windows/not frontmost. Отчёт и три
  проверенных скриншота: `/private/tmp/beeline-qgram-v2-ui.qp4vbb/`.
- PR #67 влит в main как `e82b8e8`. Обе bucket-offset таблицы теперь checked
  `u32`, что соответствует alpha-бюджету sidecar ≤600 MiB. Формат `BLQGM003`
  занимает 185,963,243 bytes: ещё -8,388,616 bytes и -7.8 MiB builder physical
  peak относительно `BLQGM002`. Rebuild занял 19.23 с / 88.11 MiB physical peak.
  Все 136,481,293 postings/checkpoints снова совпали; paired 11-case suite дал
  19.83 → 19.93 с, все per-observation fingerprints одинаковы. 135 Rust passed /
  11 ignored, fmt и clippy зелёные.
- Signed bundle `e82b8e8` и проверенный 185,963,243-byte sidecar установлены.
  PID 62291 запущен hidden/0 windows/not frontmost, SHA build/install совпадает,
  sidecar загрузился без rebuild. Warm prewarm 17 мс, FSEvents catch-up 182 мс,
  overlay replay 973 мс, full diff skipped; через 22 секунды physical footprint
  43 MiB, startup peak 78 MiB. Rollback bundle:
  `/private/tmp/Beeline-before-qgram-u32-20260905.app`; предыдущий sidecar:
  `/private/tmp/beeline-home-qgram-v2-20260905.qgram`.
- PR #68 влит в main как `6eb1e9f`. Durable overlay journal теперь сохраняет
  один collision-proof logical marker на dirty Junk directory вместо каждого
  изменённого child path; Normal/Hidden paths и первый Junk component остаются
  точными. Marker содержит невозможный в имени файла NUL и при replay напрямую
  восстанавливает dirty directory даже после изменения Junk patterns. Старый
  journal атомарно coalesce-ится при load. На production-копии: 49,838 → 8,983
  records, 6,951,098 → 1,145,256 bytes, release apply 960 → 305 мс. Первый
  migration load+apply около 594 мс; final Item count и все 560 dirty directories
  совпали. 138 Rust passed / 12 ignored, fmt, clippy, typecheck и lint зелёные.
- Signed bundle `6eb1e9f` установлен, backup:
  `/private/tmp/Beeline-before-junk-journal-coalescing-20260905.app`; исходный
  journal: `/private/tmp/beeline-overlay-before-coalescing-20260905.ndjson`.
  Реальный первый startup уплотнил 50,646 records / 7,056,118 bytes до 9,670 /
  1,239,666, несмотря на 11,283 новых catch-up paths. Replay применил 4,076
  logical entries за 617 мс против 973 мс предыдущего старта с меньшим catch-up.
  Q-gram loaded без rebuild, prewarm 16 мс, full diff skipped. PID 75911 hidden,
  0 windows/not frontmost; после Junk drain physical footprint 124 MiB, CPU 0%.
- PR #69 влит в main как `a96303f`. Path-aware fuzzy verification использует
  существующий bounded worker pool уже от 20k candidates; дешёвый name-only путь
  сохраняет порог 50k. Два paired 11-case suite: 19.55 → 16.00 с и 19.65 →
  15.86 с. `work wip` p95 45.99 → 11.97 мс и 46.11 → 11.60 мс; user CPU
  +2.2%/+0.9%, instructions +0.2%, все per-observation fingerprints совпали.
  139 Rust passed / 12 ignored, fmt и clippy зелёные.
- Signed bundle `a96303f` установлен, backup:
  `/private/tmp/Beeline-before-path-parallel-20260905.app`. Независимый GUI-pass
  `work wip` подтвердил `/Users/kiri110k/work/wip` rank 1 и стабильные 50 rows
  без дублей; PID 79444 после Escape hidden/0 windows/not frontmost. На живом
  overlay сейчас 59,997 candidates: working-set paint 19 мс, complete backend
  25 мс, complete paint 44 мс. Footprint после UI 94 MiB; отчёт и скриншот:
  `/private/tmp/beeline-path-parallel-ui.uq63ZW/`.
- PR #70 влит в main как `3525252`. Ordinary multi-token matcher сохраняет уже
  вычисленное качество final name token, переиспользует его в Path Interpretation
  и пропускает ancestor walk только когда active ranker weights доказывают, что
  ordinary score не может проиграть. Focused 500× `status report`: total p95
  53.03 → 51.19 мс, verify 46.37 → 44.61 мс; full suites быстрее на 0.3–0.6%,
  user CPU ниже на 2.0–3.7%, fingerprints одинаковы. Config-sensitive тест
  сохраняет scoped победителя при других весах. 140 Rust passed / 12 ignored.
- Signed bundle `3525252` установлен, backup:
  `/private/tmp/Beeline-before-multi-path-dominance-20260905.app`. GUI-pass
  `status report`: первые восемь rows — подходящие `status-report_*.xlsx`, первый
  exact path корректен, stale/duplicates/crash/focus loss нет. На live overlay с
  189,864 candidates working-set paint 16 мс, первая global wave 44 мс, complete
  paint 85 мс. PID 92802 после Escape hidden/0 windows/not frontmost, physical
  footprint 45 MiB, CPU 0%. Отчёт и скриншот:
  `/private/tmp/beeline-multi-dominance-ui.hYvtbf/`.
- `experiment/fuzzy-path-ten-shards` (`10d6b02`) отклонён. Десять path fuzzy
  workers вместо восьми ухудшили все case p95: `work wip` 11.30 → 13.17 мс,
  `status report` 47.32 → 48.47 мс; real wall +7.3%, user CPU +2.7%, physical
  peak +8.10 MiB. Код оставлен только в remote-ветке; отрицательный verdict:
  `prototypes/fuzzy-path-ten-shards/RESULTS.md`.
- PR #71 влит в main как `0b52143`. Временные q-gram build shards теперь хранят
  двухбайтовый local bucket и varint-дельту монотонного slot вместо фиксированных
  шести байт. На полном production index пик временных parts снизился примерно
  с 797.42 до 438.33 MiB (-45.0%), physical peak — с 88.11 до 67.8 MiB (-23%).
  Три rebuild заняли 17.46–18.69 с против прежних 19.23 с. Все три результата
  побайтно совпали с установленным `BLQGM003` sidecar: 185,963,243 bytes и SHA-256
  `90be476b47b843840b9a2c3007dc0361ca83ce84fe8077139d99c0350aa43945`.
  Финальный sidecar format и query path не менялись. Полный checkpoint: 142 Rust
  passed / 12 ignored, clippy `-D warnings`, frontend contracts, typecheck, build
  и lint зелёные. Подробности: `prototypes/qgram-delta-shard-records/RESULTS.md`.
- `experiment/qgram-skip-stride-128` (`e3dcb50`) отклонён. Удвоение расстояния
  между checkpoints уменьшило sidecar на 8,503,760 bytes (-4.6%), но ordered-tail
  literal retrieval стабильно вырос с 0.18–0.19 до 0.32 мс p95. Suite CPU и wall
  не улучшились воспроизводимо; все 2,200 наблюдений сохранили точные candidates,
  posting visits, target ranks и top-10 fingerprints. При текущих 185,963,243
  bytes из бюджета 600 MiB такой обмен не нужен. Код оставлен только в remote-
  ветке; отрицательный verdict: `prototypes/qgram-skip-stride-128/RESULTS.md`.
- Signal points, saturation/aging/transfer curves Search Memory и frequency/
  recency curve General Usage вынесены в тот же strict `ranker.json`; активный
  snapshot применяется и при startup pruning. Успешный batch Copy/Move теперь
  обучает Item после фактического завершения. Обычный same-volume Move переносит
  learned identity на collision-resolved destination; EXDEV copy+delete считается
  новым объектом и намеренно не наследует прежнюю query association. Trash/Delete
  по-прежнему ничего не обучают. Чекпоинт: 127 Rust passed / 9 ignored, clippy,
  typecheck, lint и все frontend contract tests зелёные.

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
- [Prototype end-to-end Search v2 performance](https://github.com/Kiri110K/beeline/issues/54)
  завершён на production v4 из 5 244 905 Items. Full mmap оставлен baseline:
  опечатки стабилизировали top-10 за 68–152 мс, составные запросы — за
  434–603 мс и завершались до 1,13 с. Следующий последовательный q-gram
  sidecar дал Working Set 0,36–1,48 мс p95, первый полезный global результат
  не позже 39,62 мс p95 и финальный top-50 не позже 136,05 мс p95. Все десять
  целей остались в top-50; `метолология` стабилизировалась за 14,98 мс p95.
  Sidecar пока большой: 522 142 260 байт, build 14,79 с и 584 МБ peak physical
  footprint. Тёплый query-run — 55 МБ peak physical footprint. FST не делать,
  пока integrated IPC/render или размер после сжатия не докажут необходимость.
  Архитектура первой интеграции: полный Working Set, production exact/layout
  волна, затем q-gram fuzzy supplement. Полный отчёт и воспроизводимый harness:
  `/Users/kiri110k/lab/beeline/prototypes/search-v2-performance/RESULTS.md`.
- Follow-up после живого paint-прогона устранил два интеграционных дефекта.
  Для составного запроса с пустой Working Set production сначала пересекает
  literal postings последнего token и полностью проверяет узкий ordered-tail
  pool; полный typo-safe q-gram retrieval затем всё равно выполняется и сохраняет
  recall. На reconciled индексе `vault methodology` теперь даёт целевой Item за
  3,40 мс p95 вместо 191,78 мс. Источники Working Set доходят до единого ranker
  раздельными Candidate Evidence, а прямой all-tokens-in-name match больше не
  проигрывает случайной смеси filename + ancestor. В 20 randomized samples все
  десять целей остались в final top-50, `skills-drafts` был rank 1, датированный
  `status-report` — rank 8; worst first-useful p95 32,33 мс, worst final p95
  372,91 мс, peak physical footprint 89 048 264 bytes. Eager shared dense path
  masks отдельно проверены и отвергнуты как регрессия; остаются lazy shard-local.
  Foreground recheck exact build затем завершён: `skills-drafts` и Graphify
  methodology были rank 1, датированный `status-report` — rank 8. First useful
  paint: 18 мс для `skills`, 56 мс для `vault methodology` после пустой Working
  Set волны в 20 мс и 13 мс для `status report`. Stale rows, duplicate paths и
  неверного target ordering не обнаружено. Broad final для двух последних
  запросов не успел завершиться в окне наблюдения, что совпало с headless tail.
- Этот foreground pass нашёл long-session деградацию: global retrieval добавлял
  весь исторический high-water mutable overlay, включая tombstones. В живом
  процессе `skills` получил 1 901 798 candidates вместо примерно 346k после
  свежего reconcile. Overlay теперь отдаёт search только live slots, повторно
  использует освобождённые Item/directory holes, удаляет старую parent membership
  и обрезает удалённый хвост. App-data subtree самой Beeline исключён из watcher,
  поэтому telemetry/journal writes больше не подают события обратно в индекс.
  Регрессии покрывают churn, reuse, смену parent и live-slot iterator.
- После фикса reconciled overlay содержал 335 015 slots. В 30/30 targeted
  observations все цели найдены: `work wip` rank 1, `vault methodology` rank 5
  в headless empty-memory state, status workbook rank 8. Candidate pools — 369k
  и около 503k, а не 1,9M; first-useful p95 — 0,59/3,79/0,99 мс, final p95 —
  198,32/211,09/420,74 мс. Обновлённый signed bundle установлен и тихо запущен
  с `--hidden`: 74,8 MiB settled physical footprint, 105,5 MiB startup peak,
  окон нет. Полный Rust suite: 109 passed, 8 ignored; clippy `-D warnings` и все
  frontend contract tests зелёные. Backup предыдущего приложения:
  `/private/tmp/Beeline-before-overlay-fix.app`.
- Продолжение long-idle проверки локализовало оставшуюся нагрузку. За 30 секунд
  FSEvents дал 194 уведомления по 55 путям; ни одного внутри app-data Beeline или
  его предков. Основные источники — T3 trace/LevelDB, браузерные кэши, Telegram,
  WhatsApp и Teams. Но старый Junk worker после 30 секунд непрерывного потока
  принудительно запускал clear-and-recrawl. Четыре таких прохода заняли 43,00;
  1 188,84; 1 288,17 и 305,51 секунды, а обычный watcher ждал write-lock до
  24,96 секунды.
- Локально Junk refresh больше не срабатывает по принудительному deadline:
  каждое событие заново отсчитывает пять секунд настоящей тишины. Сам refresh
  сравнивает только непосредственных детей каждой dirty directory, сохраняет
  стабильные slots неизменившихся поддеревьев, индексирует только новые
  каталоги и корректно обрабатывает смену file↔directory. Старый полный
  clear-and-recrawl и мёртвый `clear_children` удалены.
- Read-only release-проверка на persisted production base из 5 244 905 Items
  обработала целиком `~/Library/Application Support` как dirty Junk root за
  57,94 мс и нашла пять новых непосредственных Items. Обновлённый signed bundle
  установлен скрыто; после startup physical footprint 60,6 MiB, peak 66,8 MiB.
  За первые три минуты на батарее было 82 batch за 605 мс суммарно и ни одного
  Junk refresh. Зелёные: 112 Rust tests, 9 ignored, отдельный live ignored test,
  clippy `-D warnings`, frontend contracts, lint, typecheck и production build.
  Предыдущая сборка: `/private/tmp/Beeline-before-junk-refresh.app`.
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
- [Prototype staged result-stream behavior](https://github.com/Kiri110K/beeline/issues/46)
  закрыт и интегрирован в production Search v2.
  Progressive merge сохраняет по stable identity только Focused Item после
  явной result navigation; auto-focused первый Item до навигации не sticky.
  Остальной список свободно rerank; hover и scroll ничего не закрепляют и не
  обучают. Query change сбрасывает сохранение. Stream сообщает local-ready,
  global-running и complete; до 150 мс индикатора нет, после — Status Strip.
  Действующие бюджеты: UI response на keystroke ≤8 мс, first Search Results
  end-to-end ≤50 мс. Прототип #54 подтвердил три backend-волны и отсутствие
  индикатора на тёплом пути. Integrated UI проверен: `метолология` дала first
  useful paint 17 мс и complete 51 мс; `ьуерщвщдпн` — first global 36 мс и
  complete 288 мс во время startup diff-rescan; `work wip` — first useful 17 мс
  и правильный ordered path rank 1.

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
