import type { AppConfig } from '../stores/settingsStore';

/**
 * Backend configs written before `elyby_skins_in_game` existed omit the field.
 * The Rust side already applies `#[serde(default = "...")]`; this keeps the
 * renderer's local copy consistent if it ever receives an older payload (for
 * example from a cached/older backend during an in-place update).
 */
export function withConfigDefaults(config: AppConfig): AppConfig {
  return {
    ...config,
    elyby_skins_in_game: config.elyby_skins_in_game ?? true,
  };
}
