import { describe, expect, it } from 'vitest';
import { en } from '../../lib/i18n/en';
import { ru } from '../../lib/i18n/ru';
import {
  DEFAULT_SETTINGS_CATEGORY,
  SETTINGS_CATEGORY_IDS,
  SETTINGS_CATEGORY_LABEL_KEYS,
  SETTINGS_INVENTORY,
  SETTINGS_NON_UI_CONFIG_KEYS,
  SETTINGS_UI_CONFIG_KEYS,
  initialSettingsCategory,
  type SettingsConfigKeysCovered,
} from './settingsLayout';

describe('settings information architecture', () => {
  it('starts on the general category and keeps it first in the tab row', () => {
    expect(SETTINGS_CATEGORY_IDS).toContain(DEFAULT_SETTINGS_CATEGORY);
    expect(SETTINGS_CATEGORY_IDS[0]).toBe(DEFAULT_SETTINGS_CATEGORY);
    expect(DEFAULT_SETTINGS_CATEGORY).toBe('general');
    expect(initialSettingsCategory()).toBe(DEFAULT_SETTINGS_CATEGORY);
    // Experimental and technical/informational sections are never the landing
    // category: they are opened deliberately, not by default.
    expect(DEFAULT_SETTINGS_CATEGORY).not.toBe('experimental');
    expect(DEFAULT_SETTINGS_CATEGORY).not.toBe('diagnostics');
    expect(DEFAULT_SETTINGS_CATEGORY).not.toBe('about');
  });

  it('places everyday categories before experimental and technical ones', () => {
    expect(SETTINGS_CATEGORY_IDS.slice(0, 4)).toEqual([
      'general',
      'appearance',
      'java',
      'network',
    ]);
    expect(SETTINGS_CATEGORY_IDS.slice(-2)).toEqual(['diagnostics', 'about']);
    expect(SETTINGS_CATEGORY_IDS.indexOf('experimental')).toBeGreaterThan(
      SETTINGS_CATEGORY_IDS.indexOf('network'),
    );
  });

  it('assigns every setting to exactly one category', () => {
    const ids = SETTINGS_INVENTORY.map((entry) => entry.id);
    expect(new Set(ids).size).toBe(ids.length);

    for (const category of SETTINGS_CATEGORY_IDS) {
      expect(SETTINGS_INVENTORY.some((entry) => entry.category === category)).toBe(true);
    }
  });

  it('covers every dedicated preference-store row', () => {
    const ids = new Set(SETTINGS_INVENTORY.map((entry) => entry.id));
    const preferences = [
      'skin_refresh_mode',
      'toast_success',
      'toast_warning',
      'toast_error',
      'theme',
      'language',
      'font',
      'console_font',
      'liquid_glass',
      'wallpaper',
      'content_cards',
      'reduce_interface_effects',
    ];
    for (const id of preferences) {
      expect(ids.has(id), `preference ${id}`).toBe(true);
    }
  });

  it('classifies every AppConfig field as Settings UI or backend-only', () => {
    // The type only equals `true` while the two key lists together cover the
    // whole AppConfig interface; adding a config field breaks compilation here.
    const covered: SettingsConfigKeysCovered = true;
    expect(covered).toBe(true);
  });

  it('covers every Settings-editable AppConfig field exactly once', () => {
    for (const key of SETTINGS_UI_CONFIG_KEYS) {
      const entries = SETTINGS_INVENTORY.filter((entry) => entry.id === key);
      expect(entries, `config key ${key}`).toHaveLength(1);
    }
  });

  it('keeps backend-only and instance-editor config fields out of Settings', () => {
    const ids = new Set(SETTINGS_INVENTORY.map((entry) => entry.id));
    for (const key of SETTINGS_NON_UI_CONFIG_KEYS) {
      expect(ids.has(key), `non-UI config key ${key}`).toBe(false);
    }
  });

  it('keeps the Rooms switch alone in the experimental category', () => {
    const rooms = SETTINGS_INVENTORY.filter((entry) => entry.id === 'rooms_enabled');
    expect(rooms).toHaveLength(1);
    expect(rooms[0].category).toBe('experimental');
    expect(
      SETTINGS_INVENTORY.filter((entry) => entry.category === 'experimental')
        .map((entry) => entry.id),
    ).toEqual(['rooms_enabled']);
  });

  it('has a localized tab label for every category in both languages', () => {
    for (const category of SETTINGS_CATEGORY_IDS) {
      const key = SETTINGS_CATEGORY_LABEL_KEYS[category];
      expect(en[key]?.trim(), `en ${key}`).toBeTruthy();
      expect(ru[key]?.trim(), `ru ${key}`).toBeTruthy();
      // A Russian label that is byte-identical to English is a missing
      // translation, not a valid one (none of the category names is a brand).
      expect(ru[key], `ru ${key}`).not.toBe(en[key]);
    }
  });

  it('localizes the new section headings in both languages', () => {
    for (const key of ['settings.group_launch', 'settings.group_skins'] as const) {
      expect(en[key]?.trim(), `en ${key}`).toBeTruthy();
      expect(ru[key]?.trim(), `ru ${key}`).toBeTruthy();
    }
  });

  it('does not repeat a tab name as the section heading', () => {
    const pairs = [
      ['settings.section_diagnostics', 'settings.diagnostics_heading'],
      ['settings.section_experimental', 'settings.group_experimental'],
    ] as const;
    for (const [tab, heading] of pairs) {
      expect(en[heading], `en ${heading}`).not.toBe(en[tab]);
      expect(ru[heading], `ru ${heading}`).not.toBe(ru[tab]);
    }
  });
});
