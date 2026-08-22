# Design the ranking search

Type: grilling
Status: open
Blocked by: 07, 09

## Question

How does Search find and rank everything Kirill will look for — fast, layout-tolerant, and including hidden locations? Decide the engine (own index, live Spotlight queries, or a hybrid), how hidden locations become searchable, protection against junk-heavy directories, the ranking formula and its inputs (Visit Journal, Known Places, Alias Dictionary, hidden penalty, direct name matches), what exactly the Visit Journal records, and measurable latency budgets with progressive result arrival.

## Comments

Constraints and inputs already settled elsewhere:

- Ticket 07: Search Results combine exact paths, visited Locations, current-Location matches, and global Spotlight files and directories. RU/EN layout correction is required; phonetic transliteration is not. Results arrive progressively and the focused row stays stable once keyboard navigation starts.
- Ticket 09: there is no sidebar; Known Places are a ranking boost; the Alias Dictionary recommends and never excludes; hidden Items are penalized rather than excluded, and a direct name match outweighs the penalty; the Visit Journal (entered Locations and file opens) exists solely as ranker input.
- Measured 2026-08-22 on this Mac (macOS 26.5.1): Spotlight keeps index records for files under dot-directories — `mdls` returns attributes for `~/.claude/CLAUDE.md` — but neither global nor scoped `mdfind` name queries return them. Hidden-location search therefore needs the application's own crawl or index.
- Kirill's first idea for hidden scope is a whitelist grown by explicit visits: type `~/.claude/`, nothing is found by name, and the place becomes searchable afterward. This is explicitly a starting point, not a decision, and needs its own grilling. Junk-heavy directories such as agent session stores must not be indexed wholesale; their files are only ever fetched by exact identifier.
- Kirill expects search to be one of the largest and hardest parts of the project.
