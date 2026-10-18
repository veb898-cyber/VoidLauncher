import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { logUiMessage } from '../lib/uiLog';
import { t } from '../lib/i18n';
import { friendlyErrorMessage } from '../lib/errors';
import type { LoaderCheckResult } from '../components/launch/LoaderInstallModal';

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
  loader: 'Vanilla' | 'Fabric' | 'Forge' | 'NeoForge';
  loader_version: string | null;
  loader_profile: LoaderProfile | null;
  memory_mb: number | null;
  jvm_args: string[] | null;
  /** GC preset: "standard" | "g1gc" | "zgc" (undefined/null = default to "g1gc") */
  gc_preset?: string | null;
  java_path: string | null;
  resolution: { width: number; height: number } | null;
  icon: string | null;
  banner: string | null;
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
  /** Pending loader install: instance name + check result. If the launch
   *  started from a saved server, the auto-join target is kept so the launch
   *  resumes with it after the loader is installed. */
  pendingLoaderInstall: {
    instanceName: string;
    check: LoaderCheckResult;
    serverAddress?: string;
    serverPort?: number;
  } | null;

  loadInstances: () => Promise<void>;
  createInstance: (name: string, mcVersion: string, loader?: string, loaderVersion?: string) => Promise<void>;
  deleteInstance: (name: string) => Promise<void>;
  selectInstance: (name: string | null) => void;
  /** Launches the instance. Resolves `true` when the launch was accepted;
   *  `false` when it failed (error is set) or is waiting for the user to
   *  install the loader (pendingLoaderInstall is set). */
  launchGame: (instanceName: string, serverAddress?: string, serverPort?: number) => Promise<boolean>;
  dismissLoaderInstall: () => void;
  installVersion: (versionUrl: string, instanceId?: string) => Promise<string>;
  checkInstalled: (instanceName: string) => Promise<boolean>;
  saveInstance: (instance: Instance) => Promise<void>;
  clearError: () => void;
}

function reportInstanceError(message: string) {
  logUiMessage(message, 'error', 'instance');
}

function reportLaunchStatus(message: string | null) {
  if (message) logUiMessage(message, 'info', 'launch');
}

/**
 * In-flight launch guard, keyed by instance name. A double-click (or two
 * callers racing) must not fire `cmd_launch_game` twice for the same
 * instance — the backend also reserves the slot, this just avoids the
 * duplicate round-trip and duplicate error toasts.
 */
const launchingInstances = new Set<string>();

export const useInstanceStore = create<InstanceState>((set) => ({
  instances: [],
  selectedInstance: null,
  isLoading: false,
  error: null,
  isLaunching: false,
  launchStatus: null,
  pendingLoaderInstall: null,

  loadInstances: async () => {
    set({ isLoading: true });
    try {
      const instances = await invoke<Instance[]>('cmd_list_instances');
      set({ instances, isLoading: false });
    } catch (e: any) {
      const msg = friendlyErrorMessage(e, t);
      reportInstanceError(msg);
      set({ error: msg });
    }
  },

  createInstance: async (name: string, mcVersion: string, loader?: string, loaderVersion?: string) => {
    set({ isLoading: true, error: null });
    try {
      await invoke('cmd_create_instance', { name, mcVersion, loader, loaderVersion });
      const instances = await invoke<Instance[]>('cmd_list_instances');
      set({ instances, isLoading: false });
    } catch (e) {
      set({ isLoading: false });
      throw e;
    }
  },

  deleteInstance: async (name: string) => {
    try {
      await invoke('cmd_delete_instance', { name });
      const instances = await invoke<Instance[]>('cmd_list_instances');
      set({ instances, selectedInstance: null });
    } catch (e: any) {
      const msg = e.toString();
      reportInstanceError(msg);
      set({ error: msg });
    }
  },

  selectInstance: (name: string | null) => {
    set({ selectedInstance: name });
  },

  launchGame: async (instanceName: string, serverAddress?: string, serverPort?: number) => {
    if (launchingInstances.has(instanceName)) return false;
    launchingInstances.add(instanceName);

    // Clear a stale error so callers can reliably tell this launch's outcome
    // apart from an earlier failure.
    set({ isLaunching: true, launchStatus: 'Checking instance...', error: null });
    reportLaunchStatus('Checking instance...');

    try {
      try {
        // Check if loader needs to be installed
        const check = await invoke<LoaderCheckResult>('cmd_check_instance_loader', { instanceName });
        if (check.needs_install) {
          set({
            isLaunching: false,
            launchStatus: null,
            pendingLoaderInstall: { instanceName, check, serverAddress, serverPort },
          });
          return false;
        }
      } catch {
        // If check fails, try launching anyway (will get an error from backend if truly missing)
      }

      set({ launchStatus: 'Launching...' });
      reportLaunchStatus('Launching...');
      // The window is NOT minimized here. It gets minimized only when the
      // `game_started` event arrives from the backend (see useGameEvents),
      // i.e. when the Minecraft process is actually running.
      const result = await invoke<string>('cmd_launch_game', {
        instanceName,
        serverAddress: serverAddress ?? null,
        serverPort: serverPort ?? null,
      });
      reportLaunchStatus(result);
      set({ isLaunching: false, launchStatus: result });
      return true;
    } catch (e: any) {
      const msg = friendlyErrorMessage(e, t);
      reportInstanceError(msg);
      set({ isLaunching: false, launchStatus: null, error: msg });
      return false;
    } finally {
      launchingInstances.delete(instanceName);
    }
  },

  dismissLoaderInstall: () => {
    set({ pendingLoaderInstall: null });
  },

  installVersion: async (versionUrl: string, instanceId?: string) => {
    set({ isLoading: true, launchStatus: 'Downloading version...' });
    reportLaunchStatus('Downloading version...');
    try {
      const versionId = await invoke<string>('cmd_install_version', {
        versionUrl,
        instanceId: instanceId || '',
      });
      set({ isLoading: false, launchStatus: null });
      return versionId;
    } catch (e: any) {
      const msg = friendlyErrorMessage(e, t);
      reportInstanceError(msg);
      set({ error: msg, isLoading: false, launchStatus: null });
      throw e;
    }
  },

  saveInstance: async (instance: Instance) => {
    try {
      await invoke('cmd_save_instance', { instance });
      const instances = await invoke<Instance[]>('cmd_list_instances');
      set({ instances });
    } catch (e: any) {
      const msg = e.toString();
      reportInstanceError(msg);
      set({ error: msg });
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
