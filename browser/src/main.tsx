import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles.css";

const root = document.querySelector<HTMLDivElement>("#app");
if (!root) throw new Error("Application root is missing.");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
