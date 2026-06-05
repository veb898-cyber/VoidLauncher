import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

export interface LoaderLibrary {
  name: string;
  url: string;
  path: string;
  sha1: string | null;
  size: number | null;
}

export interface LoaderProfile {
  main_class: string;
  libraries: LoaderLibrary[];
  jvm_args: string[];
  game_args: string[];
}

export interface Instance {
  name: string;
  mc_version: string;
  loader: 'Vanilla' | 'Fabric' | 'Quilt' | 'Forge' | 'NeoForge';
  loader_version: string | null;
  loader_profile: LoaderProfile | null;
  memory_mb: number | null;
  jvm_args: string[] | null;
  /** GC preset: "standard" | "g1gc" | "zgc" (undefined/null = default to "g1gc") */
  gc_preset?: string | null;
  java_path: string | null;
  resolution: { width: number; height: number } | null;
  icon: string | null;
  created_at: string;
  last_played: string | null;
  play_time_seconds: number;
  notes: string;
}

interface InstanceState {
  instances: Instance[];
  selectedInstance: string | null;
  isLoading: boolean;
  error: string | null;
  isLaunching: boolean;
  launchStatus: string | null;

  loadInstances: () => Promise<void>;
  createInstance: (name: string, mcVersion: string) => Promise<void>;
  deleteInstance: (name: string) => Promise<void>;
  selectInstance: (name: string | null) => void;
  launchGame: (instanceName: string) => Promise<void>;
  installVersion: (versionUrl: string, instanceId?: string) => Promise<string>;
  checkInstalled: (instanceName: string) => Promise<boolean>;
  saveInstance: (instance: Instance) => Promise<void>;
  clearError: () => void;
}

export const useInstanceStore = create<InstanceState>((set) => ({
  instances: [],
  selectedInstance: null,
  isLoading: false,
  error: null,
  isLaunching: false,
  launchStatus: null,

  loadInstances: async () => {
    set({ isLoading: true });
    try {
      const instances = await invoke<Instance[]>('cmd_list_instances');
      set({ instances, isLoading: false });
    } catch (e: any) {
      set({ error: e.toString(), isLoading: false });
    }
  },

  createInstance: async (name: string, mcVersion: string) => {
    set({ isLoading: true, error: null });
    try {
      await invoke('cmd_create_instance', { name, mcVersion });
      const instances = await invoke<Instance[]>('cmd_list_instances');
      set({ instances, isLoading: false });
    } catch (e: any) {
      set({ error: e.toString(), isLoading: false });
    }
  },

  deleteInstance: async (name: string) => {
    try {
      await invoke('cmd_delete_instance', { name });
      const instances = await invoke<Instance[]>('cmd_list_instances');
      set({ instances, selectedInstance: null });
    } catch (e: any) {
      set({ error: e.toString() });
    }
  },

  selectInstance: (name: string | null) => {
    set({ selectedInstance: name });
  },

  launchGame: async (instanceName: string) => {
    set({ isLaunching: true, launchStatus: 'Launching...' });
    // Minimize the window immediately on click, as requested.
    // If launch fails OR the game crashes shortly after, the
    // `launch_complete` handler in useGameEvents restores the window
    // so the user actually sees the error.
    const { getCurrentWindow } = await import('@tauri-apps/api/window');
    const win = getCurrentWindow();
    let minimized = false;
    try {
      await win.minimize();
      minimized = true;
    } catch { /* window control may be unavailable */ }
    try {
      const result = await invoke<string>('cmd_launch_game', { instanceName });
      set({ isLaunching: false, launchStatus: result });
    } catch (e: any) {
      if (minimized) {
        try { await win.unminimize(); } catch { /* best-effort */ }
      }
      set({ isLaunching: false, launchStatus: null, error: e.toString() });
    }
  },

  installVersion: async (versionUrl: string, instanceId?: string) => {
    set({ isLoading: true, launchStatus: 'Downloading version...' });
    try {
      const versionId = await invoke<string>('cmd_install_version', {
        versionUrl,
        instanceId: instanceId || '',
      });
      set({ isLoading: false, launchStatus: null });
      return versionId;
    } catch (e: any) {
      set({ error: e.toString(), isLoading: false, launchStatus: null });
      throw e;
    }
  },

  saveInstance: async (instance: Instance) => {
    try {
      await invoke('cmd_save_instance', { instance });
      const instances = await invoke<Instance[]>('cmd_list_instances');
      set({ instances });
    } catch (e: any) {
      set({ error: e.toString() });
    }
  },

  checkInstalled: async (instanceName: string) => {
    try {
      return await invoke<boolean>('cmd_check_instance_installed', { instanceName });
    } catch {
      return false;
    }
  },

  clearError: () => set({ error: null }),
}));
