import React from "react";
import { createRoot } from "react-dom/client";

import App from "./App";
import KioskApp from "./kiosk/KioskApp";
import "./styles.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("missing #root container");
}

/**
 * The window shows the kiosk. `?diagnostics` opens the operator screen that
 * every earlier milestone was built against — same process, same client, same
 * core; it is simply not what a customer should ever see.
 */
const diagnostics = new URLSearchParams(window.location.search).has(
  "diagnostics",
);
if (!diagnostics) {
  document.body.classList.add("kiosk");
}

createRoot(container).render(
  <React.StrictMode>{diagnostics ? <App /> : <KioskApp />}</React.StrictMode>,
);
