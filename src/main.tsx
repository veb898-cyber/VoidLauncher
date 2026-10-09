import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";
// Side-effect import: creating the store is what applies the persisted
// appearance (normal theme or the Liquid Glass sentinel) to <html>. Without
// it the store would only be evaluated once Settings is opened, and the
// boot-time value written by the inline script in index.html would be the
// only thing in effect.
import "./stores/themeStore";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
