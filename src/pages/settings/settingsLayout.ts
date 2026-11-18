import type { MessageKey } from '../../lib/i18n/en';
import type { AppConfig } from '../../stores/settingsStore';

/**
 * Information architecture of the Settings page.
 *
 * The category order, the landing category and the setting-to-category map
 * live here instead of being spread across JSX so they can be asserted by
 * unit tests (the test environment has no DOM).
 *
 * Ordering rule: everyday launcher-wide preferences first, then appearance,
 * then Java/performance, then network, then experimental features, and the
 * technical/informational sections last.
 */
export const SETTINGS_CATEGORY_IDS = [
  'general',
  'appearance',
  'java',
  'network',
  'experimental',
  'diagnostics',
  'about',
] as const;

export type SettingsCategory = (typeof SETTINGS_CATEGORY_IDS)[number];

/**
 * The first tab the user lands on. It must be the most universal category,
 * never Experimental and never a technical tool section: Settings is opened
 * for ordinary launcher preferences far more often than for Java tuning or
 * diagnostics.
 */
export const DEFAULT_SETTINGS_CATEGORY: SettingsCategory = 'general';

/** Lazy `useState` initializer — a single source of truth for the first tab. */
export function initialSettingsCategory(): SettingsCategory {
  return DEFAULT_SETTINGS_CATEGORY;
}

export const SETTINGS_CATEGORY_LABEL_KEYS: Record<SettingsCategory, MessageKey> = {
  general: 'settings.section_general',
  appearance: 'settings.section_appearance',
  java: 'settings.section_java_performance',
  network: 'settings.section_network',
  experimental: 'settings.section_experimental',
  diagnostics: 'settings.section_diagnostics',
  about: 'settings.section_about',
};

export type SettingsEntryKind =
  /** Editable AppConfig field (saved by the page's Save button). */
  | 'config'
  /** Preference persisted by a dedicated local store. */
  | 'preference'
  /** Read-only value or folder/action button. */
  | 'action'
  /** A list or a control that runs work (Java detection, diagnostics). */
  | 'tool'
  /** Static information. */
  | 'info';

export interface SettingsEntry {
  id: string;
  category: SettingsCategory;
  kind: SettingsEntryKind;
}

/**
 * Every setting or action exposed on the Settings page, assigned to exactly
 * one category. Ids match AppConfig fields where one exists; the remaining
 * ids name the dedicated preference store or the visible action.
 */
export const SETTINGS_INVENTORY: readonly SettingsEntry[] = [
  // General — launcher-wide behaviour, skins, notifications, data.
  { id: 'after_launch_action', category: 'general', kind: 'config' },
  { id: 'skin_refresh_mode', category: 'general', kind: 'preference' },
  { id: 'elyby_skins_in_game', category: 'general', kind: 'config' },
  { id: 'toast_success', category: 'general', kind: 'preference' },
  { id: 'toast_warning', category: 'general', kind: 'preference' },
  { id: 'toast_error', category: 'general', kind: 'preference' },
  { id: 'data_dir', category: 'general', kind: 'action' },
  { id: 'game_logs', category: 'general', kind: 'action' },
  { id: 'clear_cache', category: 'general', kind: 'action' },

  // Appearance — visual identity, interface language/fonts, Liquid Glass,
  // and the optional reduction of interface effects.
  { id: 'theme', category: 'appearance', kind: 'preference' },
  { id: 'language', category: 'appearance', kind: 'preference' },
  { id: 'font', category: 'appearance', kind: 'preference' },
  { id: 'console_font', category: 'appearance', kind: 'preference' },
  { id: 'liquid_glass', category: 'appearance', kind: 'preference' },
  { id: 'wallpaper', category: 'appearance', kind: 'preference' },
  { id: 'content_cards', category: 'appearance', kind: 'preference' },
  { id: 'reduce_interface_effects', category: 'appearance', kind: 'preference' },

  // Java & performance — memory/GC defaults, the runtime, managed downloads.
  { id: 'default_memory_mb', category: 'java', kind: 'config' },
  { id: 'max_memory_mb', category: 'java', kind: 'config' },
  { id: 'default_gc_preset', category: 'java', kind: 'config' },
  { id: 'java_path', category: 'java', kind: 'config' },
  { id: 'java_installations', category: 'java', kind: 'tool' },
  { id: 'java_downloads', category: 'java', kind: 'tool' },

  // Network — proxy only; Rooms networking stays experimental.
  { id: 'proxy_enabled', category: 'network', kind: 'config' },
  { id: 'use_system_proxy', category: 'network', kind: 'config' },
  { id: 'proxy_addr', category: 'network', kind: 'config' },
  { id: 'proxy_port', category: 'network', kind: 'config' },
  { id: 'proxy_only', category: 'network', kind: 'config' },

  // Experimental — feature flags only (Rooms).
  { id: 'rooms_enabled', category: 'experimental', kind: 'config' },

  // Diagnostics and About are tools/information, not preferences.
  { id: 'diagnostics_run', category: 'diagnostics', kind: 'tool' },
  { id: 'app_version', category: 'about', kind: 'info' },
  { id: 'latest_version_check', category: 'about', kind: 'action' },
] as const;

/** AppConfig fields that are editable or actionable on the Settings page. */
export const SETTINGS_UI_CONFIG_KEYS = [
  'data_dir',
  'default_memory_mb',
  'max_memory_mb',
  'default_gc_preset',
  'java_path',
  'after_launch_action',
  'proxy_enabled',
  'proxy_addr',
  'proxy_port',
  'use_system_proxy',
  'proxy_only',
  'elyby_skins_in_game',
  'rooms_enabled',
] as const;

/**
 * AppConfig fields that intentionally have no Settings row: backend-only
 * credentials and compatibility fields, plus `default_jvm_args`, which is
 * inherited by the per-instance editor rather than edited globally.
 */
export const SETTINGS_NON_UI_CONFIG_KEYS = [
  'client_id',
  'default_jvm_args',
  'show_snapshots',
  'show_old_versions',
  'curseforge_api_key',
] as const;

type ClassifiedConfigKey =
  | (typeof SETTINGS_UI_CONFIG_KEYS)[number]
  | (typeof SETTINGS_NON_UI_CONFIG_KEYS)[number];

/**
 * Compile-time guard: adding an AppConfig field without deciding whether it
 * belongs on the Settings page (or is backend-only) makes this type `never`,
 * so the assertion in the test file stops compiling.
 */
export type SettingsConfigKeysCovered =
  Exclude<keyof AppConfig, ClassifiedConfigKey> extends never ? true : never;
