import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

interface AppConfig {
  data_dir: string;
  /** Azure client ID — only used by Rust; not editable in the UI. */
  client_id: string;
  default_memory_mb: number;
  max_memory_mb: number;
  default_gc_preset: 'standard' | 'g1gc' | 'zgc';
  default_jvm_args: string[];
  java_path: string | null;
  close_on_launch: boolean;
  show_snapshots: boolean;
  show_old_versions: boolean;
  /** CurseForge API key — only used by Rust; not editable in the UI. */
  curseforge_api_key: string;
}

interface JavaInstallation {
  path: string;
  version: string;
  major_version: number;
  is_64bit: boolean;
  vendor: string;
}

interface SettingsState {
  config: AppConfig | null;
  javaInstallations: JavaInstallation[];
  isLoading: boolean;

  loadConfig: () => Promise<void>;
  saveConfig: (config: AppConfig) => Promise<void>;
  detectJava: () => Promise<void>;
  detectSystemRam: () => Promise<number>;
}

export const useSettingsStore = create<SettingsState>((set) => ({
  config: null,
  javaInstallations: [],
  isLoading: false,

  loadConfig: async () => {
    try {
      const config = await invoke<AppConfig>('cmd_get_config');
      set({ config });
    } catch (e) {
      console.error('Failed to load config:', e);
    }
  },

  saveConfig: async (config: AppConfig) => {
    try {
      await invoke('cmd_save_config', { newConfig: config });
      set({ config });
    } catch (e) {
      console.error('Failed to save config:', e);
    }
  },

  detectJava: async () => {
    set({ isLoading: true });
    try {
      const installations = await invoke<JavaInstallation[]>('cmd_detect_java');
      set({ javaInstallations: installations, isLoading: false });
    } catch (e) {
      console.error('Failed to detect Java:', e);
      set({ isLoading: false });
    }
  },

  detectSystemRam: async () => {
    try {
      return await invoke<number>('cmd_detect_system_ram');
    } catch (e) {
      console.error('Failed to detect system RAM:', e);
      return 8192;
    }
  },
}));
