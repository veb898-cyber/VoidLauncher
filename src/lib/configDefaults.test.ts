import { describe, expect, it } from 'vitest';
import type { AppConfig } from '../stores/settingsStore';
import { withConfigDefaults } from './configDefaults';

const base = {
  data_dir: 'C:\\VoidLauncher',
  client_id: 'client',
  default_memory_mb: 4096,
  max_memory_mb: 8192,
  default_gc_preset: 'g1gc',
  default_jvm_args: [],
  java_path: null,
  close_on_launch: false,
  proxy_enabled: false,  proxy_addr: '',
  proxy_port: 0,
  show_snapshots: false,
  show_old_versions: false,
  curseforge_api_key: '',
} as unknown as AppConfig;

describe('withConfigDefaults', () => {
  it('keeps the experimental Rooms feature disabled for a legacy config', () => {
    expect(withConfigDefaults(base).rooms_enabled).toBe(false);
  });

  it('keeps an explicit Rooms opt-in', () => {
    expect(withConfigDefaults({ ...base, rooms_enabled: true }).rooms_enabled).toBe(true);
  });

  it('enables Ely.by in-game skins for a legacy config without the field', () => {
    expect(withConfigDefaults(base).elyby_skins_in_game).toBe(true);
  });

  it('keeps an explicit opt-out', () => {
    expect(
      withConfigDefaults({ ...base, elyby_skins_in_game: false }).elyby_skins_in_game,
    ).toBe(false);
  });

  it('keeps an explicit opt-in', () => {
    expect(
      withConfigDefaults({ ...base, elyby_skins_in_game: true }).elyby_skins_in_game,
    ).toBe(true);
  });

  it('enables proxy-only mode for a legacy config without the field', () => {
    expect(withConfigDefaults(base).proxy_only).toBe(true);
  });

  it('keeps an explicit proxy-only opt-out', () => {
    expect(withConfigDefaults({ ...base, proxy_only: false }).proxy_only).toBe(false);
  });

  it('maps the legacy close-on-launch flag to the new action selector', () => {
    expect(
      withConfigDefaults({ ...base, close_on_launch: true } as unknown as AppConfig)
        .after_launch_action,
    ).toBe('close');
    expect(
      withConfigDefaults({ ...base, close_on_launch: false } as unknown as AppConfig)
        .after_launch_action,
    ).toBe('do_nothing');
    expect(withConfigDefaults(base).after_launch_action).toBe('do_nothing');
  });

  it('keeps an explicit after-launch action', () => {
    expect(
      withConfigDefaults({ ...base, after_launch_action: 'minimize' }).after_launch_action,
    ).toBe('minimize');
  });

  it('defaults the system-proxy source on for legacy payloads and keeps an explicit choice', () => {
    expect(withConfigDefaults(base).use_system_proxy).toBe(true);
    expect(
      withConfigDefaults({ ...base, use_system_proxy: false }).use_system_proxy,
    ).toBe(false);
  });
});
