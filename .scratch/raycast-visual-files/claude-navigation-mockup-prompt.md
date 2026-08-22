Build the disposable interactive browser mockup requested by `.scratch/raycast-visual-files/issues/19-prototype-navigation-search-preview.md`.

Before writing anything, read these files completely:

- `CONTEXT.md`
- `.scratch/raycast-visual-files/issues/07-navigation-action-model.md`
- `.scratch/raycast-visual-files/issues/19-prototype-navigation-search-preview.md`
- `demos/GUI-VERIFICATION.md`

Inspect the existing Tauri demo UI and CSS under `demos/tauri` only as a behavioral and density reference. Also inspect these two screenshots:

- `/Users/kiri110k/.t3/userdata/attachments/7e6d479e-34f7-4ff2-a197-bd595c96441b-25ffc0ca-f7ce-4909-aa10-a81da29640b9.png`
- `/Users/kiri110k/.t3/userdata/attachments/7e6d479e-34f7-4ff2-a197-bd595c96441b-c8bb643e-e592-4b3c-9b00-67d79d3bf51e.png`

The variant prompt supplies the exact output path and design direction. Create only that one HTML file and edit nothing else. This is a disposable product-thinking artifact, not production source.

Requirements:

- One self-contained offline HTML file with inline CSS and JavaScript, no CDN, no external assets, and system fonts.
- Russian UI copy. Keep canonical domain terms such as Browse Mode, Search Mode, Navigation Input, Search Results, Reveal, Focused Item, Selected Items, Preview Panel, Quick Look, and Action Menu where they help discussion.
- Dense macOS dark UI. It should look like a serious utility, not a landing page or generic dashboard. No gradients, promotional copy, decorative animation, fake glass effects, or oversized empty space.
- Produce one coherent design, not internal A/B variants and not a theme switcher. It must be an interactive simulation rather than a static screenshot.
- Include realistic dense Recents and folder data in Russian and English. Include file and directory results, AGENTS.md under `~/work`, an image, a PDF, a Markdown file, Downloads, iCloud Drive, and a missing or unavailable Item state.
- New Tab starts with an active Navigation Input. Provide a query scenario for `agents` and the wrong-layout equivalent `фпутеы`, and a scenario for `work` / `цщкл`.
- Search Results must be a separate floating layer. The first result is focused. A single click or Enter performs Reveal: a file opens its containing Location and becomes Focused Item; a directory becomes the Location. Reveal must not launch an external app.
- Escape from Search Mode hides Search Results and restores the exact previous Browse selection while retaining the query text inactive. Clicking the Navigation Input, Command+L, or the physical Slash key reactivates it and selects all retained text.
- Implement enough real keyboard behavior to test the mental model: Up/Down, Control+J/K, Left/Right by mode, Enter, Space, Escape, Command+K, Command+L, Shift+arrows, and the physical Slash key. Avoid trapping the browser or using unsafe APIs.
- Support single selection, Shift range selection, and Command click additive selection. Enter with multi-select acts only on the Focused Item.
- Preview Panel is a fixed-width right column, enabled, instant, and has no animation. It shows image, text, PDF-like, file metadata, or directory metadata states and follows Focused Item in both Browse and Search Results.
- Quick Look is separate from Preview Panel. Simulate a full-document layer opened by Space. While open, Up/Down navigate files only. Quick Look and Action Menu cannot coexist.
- Action Menu opens through Command+K and an on-screen control. Show Item actions with a selection and application actions without one.
- Include a compact settings drawer or state sheet for the After Action table. It should make per-action Hide Window / Keep Open settings visible without turning the mockup into a settings app.
- Include loading, no-results, unavailable Item, and narrow-window states. A compact state selector is acceptable, but the main path must also be keyboard-testable.
- Make the current logical state legible without filling the product UI with developer labels. Put explanatory annotations outside the simulated app when needed.
- The page itself should contain a short note explaining the design's central layout idea and what feedback Kirill should give. Do not claim the design is best.
- Responsive enough to inspect at desktop widths down to about 820px. No dev server is allowed or needed.
- Use plain human writing. No AI filler, no em dashes, no curly quotes, no marketing language.

After writing the file, read it back and self-review for broken controls, missing required states, overflow, and accidental external dependencies. Fix what you find. In your final response, report only the output path and a terse list of what is interactive.
