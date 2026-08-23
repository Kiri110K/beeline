import { memo, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { useVirtualizer } from "@tanstack/react-virtual";
import { runBench } from "../../shared/bench-core.js";
import "../../shared/bench.css";

const Row = memo(function Row({ item, focused, y }) {
  return (
    <div
      className={focused ? "row focused" : "row"}
      style={{ transform: `translateY(${y}px)` }}
    >
      <span className="name">{item.name}</span>
      <span className="kind">{item.kind}</span>
      <span className="size">{item.size}</span>
    </div>
  );
});

let api = null;

function App() {
  const [items, setItems] = useState([]);
  const [focus, setFocus] = useState(0);
  const parentRef = useRef(null);

  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 28,
    overscan: 10,
  });

  useEffect(() => {
    api = {
      setItems,
      setFocus,
      scrollToIndex: (i) => virtualizer.scrollToIndex(i),
      scrollElement: () => parentRef.current,
    };
  });

  return (
    <div className="scroller" ref={parentRef}>
      <div className="inner" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((v) => {
          const it = items[v.index];
          return it ? (
            <Row key={it.id} item={it} focused={v.index === focus} y={v.start} />
          ) : null;
        })}
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")).render(<App />);

const adapter = {
  setup(items) {
    flushSync(() => {
      api.setItems(items);
      api.setFocus(0);
    });
  },
  setFocus(i) {
    flushSync(() => api.setFocus(i));
    api.scrollToIndex(i);
  },
  appendRows(rows) {
    flushSync(() => api.setItems((prev) => [...prev, ...rows]));
  },
  swapItems(items) {
    flushSync(() => api.setItems(items));
  },
  scrollElement: () => api.scrollElement(),
};

(async function start() {
  while (!api) {
    await new Promise((r) => requestAnimationFrame(r));
  }
  runBench(adapter);
})();
