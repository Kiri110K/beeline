# Research Raycast v2 extension surface

Type: research
Status: resolved
Blocked by: none

## Question

Using current official Raycast documentation and existing extension examples, what can a Raycast v2 extension actually express for a persistent list-based file browser: top-level browser-style Tabs, Path Input, keyboard overrides, full mouse actions, Quick Look, navigation history, and per-Tab persisted state? Identify hard platform limits separately from behaviors that merely require a different Raycast-native representation.

## Comments

## Answer

Публичный API Raycast v2 по-прежнему даёт расширению набор нативных компонентов, а не произвольное окно с React DOM и CSS. Для этого продукта основой может быть `List`, но точную браузерную оболочку внутри расширения собрать нельзя.

| Потребность | Что даёт публичный API | Вывод |
| --- | --- | --- |
| Вкладки сверху | В [`List`](https://developers.raycast.com/api-reference/user-interface/list) есть заголовок, строка поиска и один `List.Dropdown` справа от неё. В полном [перечне UI-компонентов](https://developers.raycast.com/api-reference/user-interface) нет `Tabs`, `TabView` или произвольного header-компонента. | Настоящая полоса вкладок сверху является жёстким ограничением платформы. Активную вкладку можно представить через `List.Dropdown`, отдельную секцию списка или действия переключения, но выглядеть как браузерные вкладки это не будет. |
| Ввод пути | `List` позволяет контролировать `searchText`, получать `onSearchTextChange` и отключать встроенную фильтрацию. | Строка поиска может одновременно быть адресной строкой: распознавать абсолютный путь, `~` и вставленный путь. Второе независимое поле над списком добавить нельзя. У `List` нет события submit для search bar, поэтому открытие точного пути по Enter потребует синтетической строки результата с primary action либо отдельного action shortcut. |
| Клавиатура | Расширение назначает shortcuts только действиям через [`ActionPanel`](https://developers.raycast.com/api-reference/user-interface/action-panel). [`Keyboard.KeyEquivalent`](https://developers.raycast.com/api-reference/keyboard) включает стрелки, Space, Tab и буквы. `List` даёт `selectedItemId`, `onSelectionChange` и нативную навигацию вверх/вниз, но не `onKeyDown` и не перехват сырых событий. Raycast также игнорирует зарезервированные shortcuts, что зафиксировано в [changelog API](https://developers.raycast.com/misc/changelog). | `↑/↓` уже работают нативно. `Ctrl+J/K`, `→` и Space можно попробовать назначить действиям, а выбор строки при необходимости контролировать через `selectedItemId`. Но API не обещает, что немодифицированные `→` и Space победят фокус search bar и системную навигацию. Это нужно проверить коротким прототипом. Полностью переопределить клавиатурную модель нельзя. |
| Мышь | Хост сам обрабатывает наведение, выбор, прокрутку, dropdown и клики в Action Panel. У `List.Item` есть данные и `actions`, но нет `onClick`, `onDoubleClick`, `onContextMenu` или drag handlers. | Обычная мышиная навигация остаётся нативной. Собственные single-click, double-click, right-click и drag-and-drop сценарии для строки являются жёстким ограничением. |
| Quick Look | У `List.Item` есть `quickLook`, а [`Action.ToggleQuickLook`](https://developers.raycast.com/api-reference/user-interface/actions#action.togglequicklook) открывает системный preview. Стандартный shortcut Raycast сейчас `Cmd+Y`. | Quick Look поддержан напрямую. Space можно проверить как альтернативный shortcut, но документированной гарантии для Space в сфокусированном `List` нет. |
| Навигация по папкам | [`useNavigation`](https://developers.raycast.com/api-reference/user-interface/navigation) даёт только `push` и `pop`; Escape автоматически делает pop. Нет forward, replace, чтения стека или нескольких независимых стеков. | Иерархический drill-down можно строить нативным стеком. Браузерные Back, Forward и отдельная история каждой вкладки потребуют собственной модели состояния и смены текущей папки внутри одного `List`. |
| Состояние вкладок | [`LocalStorage`](https://developers.raycast.com/api-reference/storage) хранит небольшие значения между запусками; [`useCachedState`](https://developers.raycast.com/utilities/react-hooks/usecachedstate) сохраняет JSON-сериализуемое состояние между запусками команды. `List.Dropdown.storeValue` отдельно запоминает выбранный пункт. | Список временных и закреплённых вкладок, текущую папку, selected item и собственную историю можно восстановить после закрытия или idle reset. Нативный navigation stack сам не сериализуется, его нельзя считать состоянием вкладки. |

Официальный [`Open Folders`](https://github.com/raycast/extensions/blob/main/extensions/open-folders/src/utils/file-list.tsx) показывает рабочий нативный максимум: `List` или `Grid`, `push` для входа в подпапку, `Action.Open`, Copy Path, Trash, Quick Look и pins. Он использует `Cmd+Right` для browse, а не немодифицированную стрелку, и не пытается рисовать собственные вкладки.

Практическая граница для спецификации такая: v1 может быть быстрым файловым браузером на одном `List` с адресной строкой в search bar, собственным состоянием вкладок и истории, Quick Look и полноценным Action Panel. Полоса вкладок как в браузере и собственные контекстные мышиные жесты внутри публичного Raycast Extension API недостижимы. Если они обязательны визуально, потребуется отдельное нативное окно или приложение, а не расширение Raycast.
