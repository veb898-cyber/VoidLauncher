import React from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import "./index.css";
// Side-effect import: creating the store is what applies the persisted
// appearance (normal theme or the Liquid Glass sentinel) to <html>. Without
// it the store would only be evaluated once Settings is opened, and the
// boot-time value written by the inline script in index.html would be the
// only thing in effect.
import "./stores/themeStore";
import { useLanguageStore } from "./stores/languageStore";

// The installer asks for a language before its first page, and that choice
// must be what the launcher boots with. The store persists every explicit
// choice, so a missing persisted key is exactly the "never asked" first run:
// read the installer's registry value once and seed the store from it —
// before the first render, so no wrong-language frame ever flashes. Once the
// user picks a language in Settings the key exists and this never runs again.
async function applyInstallerLanguage() {
  if (localStorage.getItem("voidlauncher.language")) return;
  try {
    const lang = await invoke<string | null>("cmd_get_installer_language");
    if (lang === "en" || lang === "ru") {
      useLanguageStore.setState({ language: lang });
    }
  } catch {
    // Not running under Tauri (preview/dev page): keep the default language.
  }
}

async function boot() {
  await applyInstallerLanguage();
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void boot();
