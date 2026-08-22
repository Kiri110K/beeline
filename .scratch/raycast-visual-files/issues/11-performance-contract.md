# Decide the performance contract

Type: grilling
Status: open
Blocked by: 06

## Question

What measurable performance budgets define success for global-shortcut entry, warm restoration, opening a new Tab, entering a directory, showing Recents, keyboard response, and Quick Look? For each budget, decide the allowed loading indicator, partial-result behavior, cancellation, caching, and fallback when local, cloud, or mounted storage cannot meet it.

## Comments

Performance is an absolute product constraint, but Kirill does not want budgets invented without evidence. The contract should derive realistic thresholds from the Tauri prototype and production-path measurements, then choose the strictest budgets that preserve the immediate feel rather than weakening the requirement to match an arbitrary implementation.
