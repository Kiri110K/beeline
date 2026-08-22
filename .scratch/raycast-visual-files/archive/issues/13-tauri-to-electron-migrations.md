# Research migrations from Tauri to Electron

Type: research
Status: resolved
Blocked by: none

## Question

Why did the OpenCode and ChatWise teams move their desktop applications from Tauri to Electron? Reconstruct each migration from primary evidence such as maintainer statements, pull requests, commits, issues, release notes, or source history. Separate explicitly stated reasons from evidence-backed inference. Identify which problems concern Tauri itself, which concern macOS or WKWebView, which were specific to the teams' product requirements, and whether Tauri v2 has since changed the relevant behavior. End with concrete implications for a shortcut-driven macOS file browser and a list of risks that must be tested rather than assumed.

## Comments

## Answer

### Короткий вывод

Эти два перехода нельзя складывать в один аргумент «Tauri не годится».

- У OpenCode есть подробное объяснение от разработчика. Electron лучше совпал с их архитектурой: продукт уже был написан на TypeScript, ему всё равно требовался Node или Bun, а Tauri запускал этот runtime отдельным CLI-sidecar. Плюс их тяжёлый интерфейс работал хуже и немного иначе в WebKit, чем в Chromium.
- У ChatWise публично подтверждён переход, но публичного объяснения причин найти не удалось. Команда отдельно утверждает, что после переписывания производительность и расход памяти остались сопоставимыми. Говорить, что ChatWise ушёл из-за WebKit, Rust, плагинов или плохой производительности Tauri, оснований пока нет.

Для нашего файлового браузера опыт OpenCode является предупреждением про WKWebView и оконное поведение, но его главный архитектурный довод почти не переносится. У нас нет большого TypeScript-сервера, который неудобно запускать как sidecar. Наоборот, обход файлов, Spotlight и системные интеграции естественно поместить в Rust или небольшой нативный модуль.

### Какие именно проекты исследованы

**OpenCode** здесь означает актуальный проект [`anomalyco/opencode`](https://github.com/anomalyco/opencode), ранее находившийся в организации SST. Electron-вариант появился отдельным пакетом в [PR #15663](https://github.com/anomalyco/opencode/pull/15663), слитом 4 марта 2026 года. Разработчик OpenCode Брендан Аллан опубликовал [объяснение перехода](https://dev.to/brendonovich/moving-opencode-desktop-to-electron-4hip) 19 апреля. CI перестал собирать Tauri-версию [3 мая](https://github.com/anomalyco/opencode/commit/e77867ef058f2e0fde159c5d6fb6b2e575f9f7a7), а [PR #25822](https://github.com/anomalyco/opencode/pull/25822) 5 мая переименовал Electron-пакет в основной `desktop`.

Это был не уход со старого Tauri v1. Последний Tauri-пакет перед отключением сборки зависел от [`tauri = 2.9.5`](https://github.com/anomalyco/opencode/blob/e77867ef058f2e0fde159c5d6fb6b2e575f9f7a7/packages/desktop/src-tauri/Cargo.toml), поверх него применялся ещё и свежий commit самого Tauri.

**ChatWise** здесь означает приложение EGOIST и его публичный release-репозиторий [`egoist/chatwise-releases`](https://github.com/egoist/chatwise-releases). Исходники приложения закрыты. Публичная история позволяет восстановить границу миграции:

- 9 марта 2026 года прежний Tauri workflow был [переименован в `release-deprecated.yml`](https://github.com/egoist/chatwise-releases/commit/84635cc02f7fc818c3ef40b01a266a433aab2a8f), его событие стало называться `release-tauri`, а sync workflow тоже получил пометку deprecated.
- В тот же день появился [новый Electron workflow](https://github.com/egoist/chatwise-releases/commit/bb01ca1248dac10517486ba4f1db7bf682413840). Он собирал закрытый пакет `desktop-new` через Node, pnpm и Bun на macOS, Windows и Linux.
- Последний публичный Tauri-релиз [`v0.10.8`](https://github.com/egoist/chatwise-releases/releases/tag/v0.10.8) от 19 марта отправляет пользователя вручную скачать новую линию v26.
- Официальная [инструкция миграции](https://docs.chatwise.app/migrate-to-v26) говорит, что начиная с `26.3.0` desktop был переписан на Electron. Она также прямо заявляет: «comparable performance and memory consumption as previous versions».

Точную версию Tauri в ChatWise v0 проверить нельзя: manifest лежит в закрытом репозитории. Нельзя подтвердить и точный день первого публичного Electron-билда только по открытым данным. Версия `26.3.0` обозначает март 2026 года, но третий компонент у ChatWise является build id, а не числом дня.

### Почему ушёл OpenCode

| Причина | Что команда сказала явно | Класс причины | Исправлено ли это в текущем Tauri v2 |
| --- | --- | --- | --- |
| WebKit медленнее Chromium на их интерфейсе | Брендан пишет, что WebKit на macOS и Linux имел «worse performance than Chromium when rendering our app». Он также упоминает различия в стилях. | WKWebView на macOS, WebKitGTK на Linux, требования сложного UI | Нет в обычной конфигурации. [Tauri по-прежнему использует WKWebView на macOS и WebKitGTK на Linux](https://v2.tauri.app/reference/webview-versions/). Это свойство выбранного webview, а не исправимый флаг Tauri. |
| Одинаковый результат на трёх ОС | Разные движки мешали команде выпускать согласованный интерфейс. Electron дал один Chromium. | Продуктовое требование и системные webview | Нет. Экспериментальная интеграция CEF развивается, но [основной tracking issue остаётся открытым](https://github.com/tauri-apps/cef-rs/issues/192), а у runtime ещё встречаются базовые [IPC-баги](https://github.com/tauri-apps/tauri/issues/15190). Считать CEF готовой заменой штатному Tauri runtime пока нельзя. |
| Запуск bundled CLI замедлял старт и иногда не удавался | Tauri-приложение запускало `opencode serve` как отдельный CLI. Команда наблюдала дополнительное время старта и редкие отказы, особенно на Windows. | Архитектура OpenCode, управление sidecar | Tauri v2 [поддерживает Node sidecar](https://v2.tauri.app/learn/sidecar-nodejs/), но sidecar всё равно надо упаковать, запустить и контролировать. Плагин не превращает его во встроенный runtime. |
| Весь продукт уже написан на TypeScript | Команда собиралась перейти с Bun на Node и могла исполнять сервер прямо во встроенном Node-процессе Electron. Брендан отдельно пишет, что Rust не дал бы выигрыша без переписывания всего ядра. | Стек и навыки команды, продуктовая архитектура | Не относится к версии Tauri. Это главный аргумент в пользу Electron именно для OpenCode. |
| Размер Electron-дистрибутива | Команда признаёт больший bundle и сознательно принимает этот обмен. | Цена Electron | Не проблема, которую они пытались решить. |

Автор прямо отвергает более широкое толкование: решение «has nothing to do with either Tauri or Electron being innately better or faster». В конце он называет Tauri хорошим вариантом для более простого UI с нативной логикой и системными API. По этому описанию наш файловый браузер ближе именно ко второму случаю, а не к OpenCode.

### Что можно сказать о ChatWise, а что нельзя

Публичные факты:

- Команда действительно заменила Tauri на Electron, а не просто добавила вторую сборку.
- Одновременно новая линия получила in-app browser для fetch, живой streaming Bash output, локальные skills и custom commands. Это видно в официальной [странице v26](https://docs.chatwise.app/migrate-to-v26).
- Новый release workflow стал проще в одном узком смысле: в нём нет Rust toolchain, `tauri-action` и системных WebKitGTK-пакетов. Это видно при сравнении [Electron workflow](https://github.com/egoist/chatwise-releases/commit/bb01ca1248dac10517486ba4f1db7bf682413840) с его публичным Tauri-предшественником.
- ChatWise заявляет о сопоставимых, а не худших, памяти и производительности после перехода. Чисел и метода измерения команда не публикует.

Остальное является только правдоподобной гипотезой. Electron мог упростить встроенный браузер, команды и Node-инструменты. Chromium мог убрать различия WebKit. Один web stack мог ускорить разработку небольшой команды. Но maintainer этого публично не сказал, а закрытая история приложения не позволяет доказать причинность. Поэтому ChatWise подтверждает, что Electron может оказаться приемлемым даже для приложения, которое продаёт себя через скорость. Он не подтверждает конкретный дефект Tauri.

### Насколько эти причины относятся к нашему файловому браузеру

| Наше требование | Что следует из миграций |
| --- | --- |
| Открытие по глобальному shortcut | Оба фреймворка имеют штатный global shortcut API: [Tauri plugin](https://v2.tauri.app/plugin/global-shortcut/) и [Electron `globalShortcut`](https://www.electronjs.org/docs/latest/api/global-shortcut). Ни одна миграция не показывает преимущество здесь. Electron сам документирует [давнюю проблему с не-QWERTY layouts на macOS](https://www.electronjs.org/docs/latest/tutorial/keyboard-shortcuts), поэтому `Super+F` надо проверять на русской и английской раскладках. |
| Минимальная задержка от shortcut до готового окна | OpenCode показывает, что запуск внешнего runtime может испортить старт Tauri. У нас такой runtime не нужен. В обоих web-вариантах следует создать окно заранее, прогреть frontend и прятать, а не уничтожать. Тогда сравнивается show, focus и первый актуальный список, а не холодный bootstrap фреймворка. |
| 10 000 строк | Это главный переносимый риск WKWebView. Виртуализированный список рисует лишь видимые строки, поэтому заявления OpenCode недостаточно. Нужен одинаковый frontend и замер scroll frame time, input latency и выделения строки в WKWebView и Chromium. |
| Spotlight и Quick Look | В Electron всё равно потребуется Node native addon, helper или системная команда. В Tauri Rust backend и нативный bridge подходят естественнее. У чистого macOS-приложения прямой доступ проще всего. OpenCode не проверял подобный workload. |
| Полная клавиатура, мышь и вкладки | Web UI даёт одинаково полный DOM control в Tauri и Electron. Возможные различия лежат в WKWebView, Chromium и поведении окна, а не в компонентной модели. |
| Низкая idle memory | Tauri имеет структурное преимущество, потому что не поставляет отдельный Chromium, но реальная память зависит от frontend, hidden window и кэшей. Утверждение ChatWise показывает, что Electron иногда может сравняться на уровне всего приложения, но без цифр это не бюджет для нашего продукта. |
| Только macOS | Главная боль OpenCode с тремя разными движками почти исчезает. Остаётся один WKWebView на поддерживаемой версии macOS. Если продукт не планируется быстро переносить на Windows и Linux, консистентность Chromium между ОС не должна определять выбор. |

### Что должны проверить три параллельных демо

Собирать Tauri, Electron и macOS-вариант параллельно разумно, если это один и тот же тест, а не три маленьких продукта. Tauri и Electron должны использовать один frontend, один набор из 10 000 записей и одинаковую виртуализацию. Каждое демо должно пройти следующие проверки на этом Mac:

1. Регистрирует `Super+F`, срабатывает в русской и английской раскладках, поверх полноэкранного приложения, после sleep/wake и после повторного запуска.
2. Держит готовое скрытое окно. Измеряется `shortcut event -> window visible`, затем `-> path input focused`, затем `-> rows painted`. Нужны median и p95 минимум по 30 вызовам.
3. Открывает папку с 10 000 entries. Измеряется время перечисления, первого отображения, поиска, перехода выделения и непрерывной прокрутки.
4. Показывает системный Quick Look по Space и закрывает его повторным Space или Escape без потери выбранной строки.
5. Выполняет один Spotlight Recents query через предполагаемый production bridge. Для Tauri это Rust или Swift FFI, для Electron Node addon или отдельный helper, для нативной версии Foundation напрямую.
6. Проверяется после потери фокуса, на другом Space, с несколькими мониторами и при быстром десятикратном вызове shortcut. Именно здесь обычно проявляются различия window activation, а не в статичном UI.
7. Снимаются idle RSS, private memory, CPU и energy impact с видимым и скрытым окном. Также фиксируются размер установленного приложения и время первого холодного запуска после reboot.

Выбор следует делать по результату этих тестов. До них Tauri остаётся сильным кандидатом: Rust здесь выполняет настоящую файловую работу, macOS одна, а idle memory важна. Electron является сильным контрольным вариантом благодаря предсказуемому Chromium, Node и зрелому оконному API. Нативная демо нужна, чтобы понять цену, которую web hosts добавляют к фокусу, Quick Look и системным интеграциям.

### Нерешённые неопределённости

- EGOIST не опубликовал причины перехода ChatWise. Без его комментария или открытой истории любые конкретные причины останутся предположением.
- OpenCode не опубликовал воспроизводимые числа WebKit против Chromium и не разделил вклад macOS WKWebView и Linux WebKitGTK. Их вывод верен для их приложения, но не является общим benchmark.
- CEF-поддержка Tauri меняется быстро. На дату исследования она ещё не выглядит production-equivalent штатному runtime; это стоит перепроверить перед реализацией, но не использовать для первой демо.
- Ни одна из двух миграций не проверяет главное для нас: hotkey-to-window latency, фокус после глобального shortcut, Quick Look и очень большой файловый список на конкретном Mac пользователя.
