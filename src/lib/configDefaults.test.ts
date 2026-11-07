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
  proxy_enabled: false,
  proxy_addr: '',
  proxy_port: 0,
  show_snapshots: false,
  show_old_versions: false,
  curseforge_api_key: '',
} as unknown as AppConfig;

describe('withConfigDefaults', () => {
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
});
