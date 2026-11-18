import { createElement, type ComponentType } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  SETTINGS_CATEGORY_IDS,
  SETTINGS_INVENTORY,
} from './settingsLayout';
import { useSettingsStore, type AppConfig } from '../../stores/settingsStore';
import { useLanguageStore } from '../../stores/languageStore';
import { useInstanceStore } from '../../stores/instanceStore';

// React's server renderer reads Zustand's `getInitialState`, not the live
// store, so `setState` alone would keep the page on its loading skeleton.
// This test is about the Settings UI, not SSR hydration: serve the populated
// state through the hook itself.
vi.mock('../../stores/settingsStore', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../stores/settingsStore')>();
  let state: Record<string, unknown> = {
    config: null,
    javaInstallations: [],
    isLoading: true,
    loadError: null,
  };
  const hook = ((selector: (value: Record<string, unknown>) => unknown) =>
    selector(state)) as unknown as typeof actual.useSettingsStore;
  hook.getState = () => state as never;
  hook.setState = ((patch: unknown) => {
    const next =
      typeof patch === 'function'
        ? (patch as (value: Record<string, unknown>) => Record<string, unknown>)(state)
        : (patch as Record<string, unknown>);
    state = { ...state, ...next };
  }) as typeof hook.setState;
  hook.subscribe = (() => () => {}) as typeof hook.subscribe;
  return { ...actual, useSettingsStore: hook };
});

// Static markup has no DOM to portal into. Rendering portal children inline
// keeps the real components (and therefore the real settings rows) intact.
vi.mock('react-dom', async (importOriginal) => {
  const actual = await importOriginal<typeof import('react-dom')>();
  return {
    ...actual,
    createPortal: (children: unknown) => children,
  };
});

function installMinimalDom(): void {
  const g = globalThis as unknown as { document?: unknown; window?: unknown };
  if (!g.document) {
    g.document = {
      documentElement: {
        lang: 'en',
        style: {},
        setAttribute: () => {},
        removeAttribute: () => {},
      },
    };
  }
  if (!g.window) {
    g.window = {
      matchMedia: () => ({
        matches: false,
        addEventListener: () => {},
        removeEventListener: () => {},
      }),
    };
  }
}

const config: AppConfig = {
  data_dir: 'C:\\VoidLauncherData',
  client_id: 'test',
  default_memory_mb: 4096,
  max_memory_mb: 8192,
  default_gc_preset: 'g1gc',
  default_jvm_args: [],
  java_path: null,
  after_launch_action: 'minimize',
  proxy_enabled: false,
  proxy_addr: '127.0.0.1',
  proxy_port: 8080,
  use_system_proxy: true,
  proxy_only: true,
  show_snapshots: false,
  show_old_versions: false,
  curseforge_api_key: '',
  elyby_skins_in_game: true,
  rooms_enabled: false,
};

function renderSettings(SettingsComponent: ComponentType): string {
  installMinimalDom();
  useLanguageStore.setState({ language: 'en' });
  useSettingsStore.setState({
    config,
    javaInstallations: [],
    isLoading: false,
    loadError: null,
  });
  useInstanceStore.setState({
    instances: [],
    selectedInstance: null,
    isLoading: false,
    error: null,
    isLaunching: false,
    launchStatus: null,
    elybySkinsPrompt: null,
  });
  return renderToStaticMarkup(createElement(SettingsComponent));
}

describe('Settings inventory is backed by rendered controls', () => {
  beforeEach(() => installMinimalDom());

  it('renders every registered setting id exactly once', async () => {
    const { Settings } = await import('../Settings');
    const html = renderSettings(Settings);
    const rendered = [...html.matchAll(/data-setting-id="([^"]+)"/g)].map((m) => m[1]);
    expect(new Set(rendered).size).toBe(rendered.length);

    const expected = SETTINGS_INVENTORY.map((entry) => entry.id).sort();
    expect(rendered.sort()).toEqual(expected);
  });

  it('renders a real tabpanel for every category', async () => {
    const { Settings } = await import('../Settings');
    const html = renderSettings(Settings);
    expect(html.match(/role="tabpanel"/g)?.length).toBe(SETTINGS_CATEGORY_IDS.length);
    for (const category of SETTINGS_CATEGORY_IDS) {
      expect(html).toContain(`id="settings-tabs-panel-${category}"`);
    }
  });
});
