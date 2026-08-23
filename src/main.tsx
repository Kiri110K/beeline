import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./index.css";
import { initializeShell } from "./shell";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("index.html is missing the #root element");
}

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

void initializeShell();
