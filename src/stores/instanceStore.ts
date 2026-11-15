import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { logUiMessage } from '../lib/uiLog';
import { t } from '../lib/i18n';
import { friendlyErrorMessage } from '../lib/errors';
import { createInstallId } from '../lib/installProgress';
import { useEventStore } from '../hooks/useGameEvents';
import { addToast } from '../components/ui/Toast';

export type ElybySkinsChoice = 'retry' | 'bypass' | 'cancel';

export interface ElybySkinsPrompt {
  kind: string;
  resolve: (choice: ElybySkinsChoice) => void;
}

export interface LaunchOptions {
  /** One-shot: the dialog's "launch without Ely.by support" choice. */
  skipElybySkins?: boolean;
  /** Caller renders its own localized error toast (e.g. ServersManager). */
  silentError?: boolean;
}

type ElybySkinsPrepareResult =
  | { status: 'not_applicable' }
  | { status: 'unsupported'; min_version: string }
  | { status: 'ready'; version: string }
  | { status: 'failed'; kind: string; detail: string };

interface ElybySkinsFailure {
  kind: string;
  detail: string;
}

/** Tauri returns the untagged `LaunchCommandError::Elyby` object as-is. */
function elybyFailureFrom(error: unknown): ElybySkinsFailure | null {
  let value: unknown = error;
  if (typeof error === 'string' && error.trim().startsWith('{')) {
    try {
      value = JSON.parse(error);
    } catch {
      return null;
    }
  }
  if (!value || typeof value !== 'object' || !('elyby_skins' in value)) return null;
  const failure = (value as { elyby_skins?: unknown }).elyby_skins;
  if (!failure || typeof failure !== 'object') return null;
  const { kind, detail } = failure as { kind?: unknown; detail?: unknown };
  if (typeof kind !== 'string') return null;
  return { kind, detail: typeof detail === 'string' ? detail : '' };
}

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

export interface InstanceSetupState {
  operation: string;
  stage: 'importing' | 'preparing' | string;
  started_at: string;
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
  /**
   * Present while the backend still considers the instance incomplete
   * (import/install/preparation in flight or interrupted). Play must stay
   * disabled; the backend refuses the launch independently.
   */
  setup_state?: InstanceSetupState | null;
}

interface InstanceState {
  instances: Instance[];
  selectedInstance: string | null;
  isLoading: boolean;
  error: string | null;
  isLaunching: boolean;
  launchStatus: string | null;
  /** Non-null while the localized Ely.by preparation dialog is open. */
  elybySkinsPrompt: ElybySkinsPrompt | null;

  loadInstances: () => Promise<void>;
  createInstance: (name: string, mcVersion: string, loader?: string, loaderVersion?: string) => Promise<void>;
  deleteInstance: (name: string) => Promise<void>;
  selectInstance: (name: string | null) => void;
  /** Launches the instance. Resolves `true` when the launch was accepted,
   *  `false` when it failed (error is set). */
  launchGame: (
    instanceName: string,
    serverAddress?: string,
    serverPort?: number,
    options?: LaunchOptions,
  ) => Promise<boolean>;
  /** Resolve the currently open Ely.by dialog and continue/cancel the launch. */
  resolveElybySkins: (choice: ElybySkinsChoice) => void;
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

interface QueuedElybyPrompt {
  kind: string;
  resolve: (choice: ElybySkinsChoice) => void;
}

/**
 * Two launches can hit an Ely.by preflight failure at the same time. The UI
 * renders one prompt at a time; queue additional prompts instead of replacing
 * the previous callback (which used to leave the first launch pending
 * forever).
 */
const elybyPromptQueue: QueuedElybyPrompt[] = [];
let activeElybyPrompt: QueuedElybyPrompt | null = null;

function pumpElybyPrompt() {
  if (activeElybyPrompt || elybyPromptQueue.length === 0) return;
  const next = elybyPromptQueue.shift()!;
  activeElybyPrompt = next;
  useInstanceStore.setState({
    elybySkinsPrompt: { kind: next.kind, resolve: next.resolve },
  });
}

export const useInstanceStore = create<InstanceState>((set) => ({
  instances: [],
  selectedInstance: null,
  isLoading: false,
  error: null,
  isLaunching: false,
  launchStatus: null,
  elybySkinsPrompt: null,

  loadInstances: async () => {
    set({ isLoading: true });
    try {
      const instances = await invoke<Instance[]>('cmd_list_instances');
      set({ instances, isLoading: false, error: null });
    } catch (e: any) {
      const msg = friendlyErrorMessage(e, t);
      reportInstanceError(msg);
      set({ error: msg, isLoading: false });
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
      set((state) => ({
        instances,
        // Decide at COMPLETION time, not at call time: the user may have
        // selected another instance while the deletion was running. Only the
        // deleted instance's own selection is cleared.
        selectedInstance: state.selectedInstance === name ? null : state.selectedInstance,
      }));
    } catch (e: any) {
      const msg = e.toString();
      reportInstanceError(msg);
      set({ error: msg });
    }
  },

  selectInstance: (name: string | null) => {
    set({ selectedInstance: name });
  },

  launchGame: async (
    instanceName: string,
    serverAddress?: string,
    serverPort?: number,
    options?: LaunchOptions,
  ) => {
    if (launchingInstances.has(instanceName)) return false;
    launchingInstances.add(instanceName);

    // Clear a stale error so callers can reliably tell this launch's outcome
    // apart from an earlier failure. The backend installs a missing mod
    // loader on the fly (no pre-launch prompt), so we go straight to launch.
    set({ isLaunching: true, launchStatus: 'Launching...', error: null });
    reportLaunchStatus('Launching...');

    try {
      let skipElybySkins = options?.skipElybySkins ?? false;

      const ask = (kind: string) =>
        new Promise<ElybySkinsChoice>((resolve) => {
          elybyPromptQueue.push({ kind, resolve });
          pumpElybyPrompt();
        });

      // Preflight lets the renderer show a localized retry/bypass/cancel
      // dialog before the long launch preparation. The actual launch still
      // re-verifies the cache; if that second check fails, its structured
      // `elyby_skins` error reopens the same dialog instead of degrading to
      // a toast.
      while (true) {
        if (!skipElybySkins) {
          const prepared = await invoke<ElybySkinsPrepareResult>(
            'cmd_prepare_elyby_skins',
            { instanceName },
          );
          if (prepared.status === 'failed') {
            const choice = await ask(prepared.kind);
            if (choice === 'cancel') {
              set({ launchStatus: null });
              return false;
            }
            if (choice === 'retry') continue;
            // One-shot only: no AppConfig/account update is written.
            skipElybySkins = true;
          }
        }

        try {
          // The window is NOT minimized here. It gets minimized only when the
          // `game_started` event arrives from the backend (see useGameEvents),
          // i.e. when the Minecraft process is actually running.
          const result = await invoke<string>('cmd_launch_game', {
            instanceName,
            serverAddress: serverAddress ?? null,
            serverPort: serverPort ?? null,
            skipElybySkins,
          });
          reportLaunchStatus(result);
          set({ launchStatus: result });
          return true;
        } catch (e) {
          const failure = skipElybySkins ? null : elybyFailureFrom(e);
          if (!failure) throw e;

          const choice = await ask(failure.kind);
          if (choice === 'cancel') {
            set({ launchStatus: null });
            return false;
          }
          if (choice === 'retry') continue;
          // One-shot bypass: retry the same launch without the agent.
          skipElybySkins = true;
        }
      }
    } catch (e: any) {
      const msg = friendlyErrorMessage(e, t);
      reportInstanceError(msg);
      set({ launchStatus: null, error: msg });
      if (!options?.silentError) {
        addToast(t('errors.launch_failed', { error: msg }), 'error');
      }
      return false;
    } finally {
      launchingInstances.delete(instanceName);
      set({ isLaunching: launchingInstances.size > 0 });
    }
  },

  resolveElybySkins: (choice) => {
    const prompt = activeElybyPrompt;
    if (!prompt) return;
    activeElybyPrompt = null;
    set({ elybySkinsPrompt: null });
    prompt.resolve(choice);
    pumpElybyPrompt();
  },

  installVersion: async (versionUrl: string, instanceId?: string) => {
    set({ isLoading: true, launchStatus: 'Downloading version...' });
    reportLaunchStatus('Downloading version...');
    const installId = createInstallId();
    useEventStore.getState().beginInstall(installId);
    try {
      const versionId = await invoke<string>('cmd_install_version', {
        versionUrl,
        instanceId: instanceId || '',
        installId,
      });
      set({ isLoading: false, launchStatus: null });
      // Normally the terminal event clears this; the timeout is a fallback
      // for a command that returned before its last event was processed.
      setTimeout(() => useEventStore.getState().clearInstall(installId), 1500);
      return versionId;
    } catch (e: any) {
      useEventStore.getState().clearInstall(installId);
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
