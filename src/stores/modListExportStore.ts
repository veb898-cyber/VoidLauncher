import { create } from 'zustand';
import {
  DEFAULT_CUSTOM_TEMPLATE,
  DEFAULT_MOD_LIST_EXPORT_OPTIONS,
  MOD_LIST_FORMATS,
  normalizeExportExtension,
  type ModListExportOptions,
  type ModListFormat,
} from '../lib/modListExport';

export const MOD_LIST_EXPORT_STORAGE_KEY = 'voidlauncher-mod-list-export';
export const MAX_CUSTOM_TEMPLATE_LENGTH = 20_000;

export interface ModListExportSettings {
  format: ModListFormat;
  options: ModListExportOptions;
  customTemplate: string;
  customExtension: string;
}

export const DEFAULT_MOD_LIST_EXPORT_SETTINGS: ModListExportSettings = {
  format: 'html',
  options: { ...DEFAULT_MOD_LIST_EXPORT_OPTIONS },
  customTemplate: DEFAULT_CUSTOM_TEMPLATE,
  customExtension: 'txt',
};

const VALID_FORMATS = new Set<string>(MOD_LIST_FORMATS);

function coerceOptions(value: unknown): ModListExportOptions {
  if (!value || typeof value !== 'object') {
    return { ...DEFAULT_MOD_LIST_EXPORT_OPTIONS };
  }
  const record = value as Partial<Record<keyof ModListExportOptions, unknown>>;
  // Only an explicit `true` enables a field; a partial/old payload never
  // turns on columns the user did not choose.
  return {
    version: record.version === true,
    authors: record.authors === true,
    url: record.url === true,
    filename: record.filename === true,
    enabled: record.enabled === true,
  };
}

/** Tolerant parser for the persisted JSON (bad data falls back to defaults). */
export function parseModListExportSettings(value: unknown): ModListExportSettings {
  if (!value || typeof value !== 'object') {
    return {
      ...DEFAULT_MOD_LIST_EXPORT_SETTINGS,
      options: { ...DEFAULT_MOD_LIST_EXPORT_OPTIONS },
    };
  }
  const record = value as Record<string, unknown>;
  const format =
    typeof record.format === 'string' && VALID_FORMATS.has(record.format)
      ? (record.format as ModListFormat)
      : DEFAULT_MOD_LIST_EXPORT_SETTINGS.format;
  const customTemplate =
    typeof record.customTemplate === 'string'
      ? record.customTemplate.slice(0, MAX_CUSTOM_TEMPLATE_LENGTH)
      : DEFAULT_MOD_LIST_EXPORT_SETTINGS.customTemplate;
  const customExtension =
    normalizeExportExtension(
      typeof record.customExtension === 'string' ? record.customExtension : '',
    ) || DEFAULT_MOD_LIST_EXPORT_SETTINGS.customExtension;

  return {
    format,
    options: coerceOptions(record.options),
    customTemplate,
    customExtension,
  };
}

/** Read settings again — used when the dialog is opened after a restart. */
export function loadModListExportSettings(): ModListExportSettings {
  try {
    const raw = localStorage.getItem(MOD_LIST_EXPORT_STORAGE_KEY);
    if (!raw) {
      return {
        ...DEFAULT_MOD_LIST_EXPORT_SETTINGS,
        options: { ...DEFAULT_MOD_LIST_EXPORT_OPTIONS },
      };
    }
    return parseModListExportSettings(JSON.parse(raw));
  } catch {
    return {
      ...DEFAULT_MOD_LIST_EXPORT_SETTINGS,
      options: { ...DEFAULT_MOD_LIST_EXPORT_OPTIONS },
    };
  }
}

function persist(settings: ModListExportSettings): void {
  try {
    localStorage.setItem(MOD_LIST_EXPORT_STORAGE_KEY, JSON.stringify(settings));
  } catch {
    // Storage may be unavailable (private mode/quota); the in-memory
    // settings still apply for this session.
  }
}

function snapshot(state: ModListExportSettings): ModListExportSettings {
  return {
    format: state.format,
    options: { ...state.options },
    customTemplate: state.customTemplate,
    customExtension: state.customExtension,
  };
}

interface ModListExportStore extends ModListExportSettings {
  setFormat: (format: ModListFormat) => void;
  setOption: (option: keyof ModListExportOptions, enabled: boolean) => void;
  setCustomTemplate: (template: string) => void;
  setCustomExtension: (extension: string) => void;
}

export const useModListExportStore = create<ModListExportStore>((set, get) => ({
  ...loadModListExportSettings(),

  setFormat: (format) => {
    if (!VALID_FORMATS.has(format)) return;
    set({ format });
    persist(snapshot(get()));
  },

  setOption: (option, enabled) => {
    set((state) => ({ options: { ...state.options, [option]: enabled } }));
    persist(snapshot(get()));
  },

  setCustomTemplate: (template) => {
    set({ customTemplate: template.slice(0, MAX_CUSTOM_TEMPLATE_LENGTH) });
    persist(snapshot(get()));
  },

  setCustomExtension: (extension) => {
    set({ customExtension: normalizeExportExtension(extension) });
    persist(snapshot(get()));
  },
}));
