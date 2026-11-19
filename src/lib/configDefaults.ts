import type { AppConfig } from '../stores/settingsStore';

/**
 * Backend configs written before `elyby_skins_in_game` existed omit the field.
 * The Rust side already applies `#[serde(default = "...")]`; this keeps the
 * renderer's local copy consistent if it ever receives an older payload (for
 * example from a cached/older backend during an in-place update).
 */
export function withConfigDefaults(config: AppConfig): AppConfig {
  // Backends from before 1.6 only know the boolean `close_on_launch`.
  // Keep that preference for a cached/older payload; the Rust side performs
  // the same migration for configs read from disk.
  const legacy = config as AppConfig & { close_on_launch?: boolean };
  const afterLaunchAction =
    config.after_launch_action ??
    (legacy.close_on_launch ? 'close' : 'do_nothing');
  return {
    ...config,
    after_launch_action: afterLaunchAction,
    elyby_skins_in_game: config.elyby_skins_in_game ?? true,
    proxy_only: config.proxy_only ?? true,
    use_system_proxy: config.use_system_proxy ?? true,
    // Experimental Rooms stays opt-in; an older backend payload or a config
    // written before the flag existed must keep the tab hidden.
    rooms_enabled: config.rooms_enabled ?? false,
  };
}
