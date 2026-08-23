import { createSignal, For } from "solid-js";
import { render } from "solid-js/web";
import { createVirtualizer } from "@tanstack/solid-virtual";
import { runBench } from "../../shared/bench-core.js";
import "../../shared/bench.css";

const [items, setItems] = createSignal([]);
const [focus, setFocus] = createSignal(0);

let parentEl;
let virtualizer;

function App() {
  virtualizer = createVirtualizer({
    get count() {
      return items().length;
    },
    getScrollElement: () => parentEl,
    estimateSize: () => 28,
    overscan: 10,
  });

  return (
    <div class="scroller" ref={parentEl}>
      <div class="inner" style={{ height: `${virtualizer.getTotalSize()}px` }}>
        <For each={virtualizer.getVirtualItems()}>
          {(v) => {
            const it = () => items()[v.index];
            return (
              <div
                class="row"
                classList={{ focused: focus() === v.index }}
                style={{ transform: `translateY(${v.start}px)` }}
              >
                <span class="name">{it()?.name}</span>
                <span class="kind">{it()?.kind}</span>
                <span class="size">{it()?.size}</span>
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}

render(() => <App />, document.getElementById("root"));

const adapter = {
  setup(list) {
    setItems(list);
    setFocus(0);
  },
  setFocus(i) {
    setFocus(i);
    virtualizer.scrollToIndex(i);
  },
  appendRows(rows) {
    setItems((prev) => [...prev, ...rows]);
  },
  swapItems(list) {
    setItems(list);
  },
  scrollElement: () => parentEl,
};

requestAnimationFrame(() => {
  runBench(adapter);
});
