// Framework-agnostic benchmark driver. Each app supplies an adapter:
//   setup(items)            -> render initial list, resolve when painted
//   setFocus(index)         -> move the focused row (discrete-input path)
//   appendRows(rows)        -> append a streamed batch
//   swapItems(items)        -> replace the array (cache revalidation)
//   scrollElement()         -> the scrolling container, for forced layout
// Metrics per operation:
//   sync  = apply + microtasks + forced layout (script/reconcile/DOM/layout)
//   paint = apply .. second requestAnimationFrame (includes vsync wait)

export function makeItems(count, offset = 0) {
  const kinds = ["Document", "Image", "Folder", "Archive", "Code"];
  const items = new Array(count);
  for (let i = 0; i < count; i++) {
    const n = offset + i;
    items[i] = {
      id: n,
      name: `item-${String(n).padStart(6, "0")}.txt`,
      kind: kinds[n % kinds.length],
      size: `${(n % 900) + 12} KB`,
    };
  }
  return items;
}

const raf = () => new Promise((r) => requestAnimationFrame(r));

function stats(samples) {
  const s = [...samples].sort((a, b) => a - b);
  const pick = (q) => s[Math.min(s.length - 1, Math.floor(q * s.length))];
  return {
    n: s.length,
    p50: +pick(0.5).toFixed(2),
    p95: +pick(0.95).toFixed(2),
    max: +s[s.length - 1].toFixed(2),
  };
}

async function measure(adapter, apply) {
  const t0 = performance.now();
  apply();
  await Promise.resolve();
  void adapter.scrollElement().offsetHeight; // force style/layout
  const sync = performance.now() - t0;
  await raf();
  await raf();
  const paint = performance.now() - t0;
  return { sync, paint };
}

export async function runBench(adapter) {
  const results = {};

  // Scenario 1: focus-move through a virtualized 50k list, 300 steps.
  await adapter.setup(makeItems(50000));
  await raf(); await raf();
  let focus = 0;
  const fmSync = [], fmPaint = [];
  for (let i = 0; i < 300; i++) {
    focus += 1;
    const { sync, paint } = await measure(adapter, () => adapter.setFocus(focus));
    fmSync.push(sync); fmPaint.push(paint);
  }
  results.focusMove = { sync: stats(fmSync), paint: stats(fmPaint) };

  // Scenario 2: streamed insertion, 1k start + 49 batches of 1k.
  await adapter.setup(makeItems(1000));
  await raf(); await raf();
  const siSync = [], siPaint = [];
  for (let b = 1; b <= 49; b++) {
    const rows = makeItems(1000, b * 1000);
    const { sync, paint } = await measure(adapter, () => adapter.appendRows(rows));
    siSync.push(sync); siPaint.push(paint);
  }
  results.streamInsert = { sync: stats(siSync), paint: stats(siPaint) };

  // Scenario 3: cache revalidation — swap the 50k array, 2% of rows changed,
  // focused row must not move. 20 repetitions.
  const base = makeItems(50000);
  await adapter.setup(base);
  adapter.setFocus(120);
  await raf(); await raf();
  let current = base;
  const rvSync = [], rvPaint = [];
  for (let r = 0; r < 20; r++) {
    const next = current.slice();
    for (let c = 0; c < 1000; c++) {
      const idx = (c * 50 + r) % 50000;
      const it = next[idx];
      next[idx] = { ...it, size: `${(r * 7 + c) % 900} KB` };
    }
    current = next;
    const { sync, paint } = await measure(adapter, () => adapter.swapItems(next));
    rvSync.push(sync); rvPaint.push(paint);
  }
  results.revalidate = { sync: stats(rvSync), paint: stats(rvPaint) };

  window.__BENCH_RESULTS = results;
  window.__BENCH_DONE = true;
  document.title = "bench-done";
  return results;
}
