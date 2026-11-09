import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import { addToast } from '../components/ui/Toast';
import { useLogStore } from '../stores/logStore';
import type { LogEntry } from '../stores/logStore';
import { useFocusStore } from '../stores/focusStore';
import { useSettingsStore, type AppConfig } from '../stores/settingsStore';
import { t } from '../lib/i18n';
import { afterLaunchWindowAction, applyAfterLaunchAction, type AfterLaunchWindowAction } from '../lib/afterLaunch';
import { CRASH_KEYS, type CrashAnalysis } from '../lib/diagnostics';
import {
  applyRunningGameEvent,
  runningSnapshotIsCurrent,
} from '../lib/runningGames';
import {
  applyInstallProgress as reduceInstallProgress,
  beginInstall as beginInstallState,
  clearInstall as clearInstallState,
  EMPTY_INSTALL_PROGRESS_STATE,
  isTerminalInstallStage,
  type InstallProgressEvent,
} from '../lib/installProgress';

interface LaunchEvent {
  instance_id: string;
  status: string;
  pid: number | null;
  exit_code: number | null;
}

interface LogPayload {
  level: string;
  source: string;
  message: string;
}

interface EventState {
  installProgress: InstallProgressEvent | null;
  activeInstallId: string | null;
  finishedInstallIds: string[];
  beginInstall: (installId: string) => void;
  applyInstallProgress: (event: InstallProgressEvent) => void;
  clearInstall: (installId: string) => void;
  /** Names of the instances whose games are currently running (may be several). */
  runningGameIds: string[];
  /** PID of each tracked run; guards against an older completion clearing a newer run. */
  runningGamePids: Record<string, number>;
  /** Incremented on every event so an older backend snapshot cannot overwrite it. */
  runningStateVersion: number;
  /** Add or remove a single instance from the running set; a stale completion PID is ignored. */
  setRunningGameId: (id: string, running: boolean, pid?: number | null) => void;
  /** Replace the whole set only if no event arrived since the request started. */
  seedRunningGameIdsIfCurrent: (ids: string[], requestVersion: number) => boolean;
}

export const useEventStore = create<EventState>((set) => ({
  ...EMPTY_INSTALL_PROGRESS_STATE,
  beginInstall: (installId) =>
    set((state) => beginInstallState(state, installId)),
  applyInstallProgress: (event) =>
    set((state) => reduceInstallProgress(state, event)),
  clearInstall: (installId) =>
    set((state) => clearInstallState(state, installId)),
  runningGameIds: [],
  runningGamePids: {},
  runningStateVersion: 0,
  setRunningGameId: (id, running, pid) =>
    set((state) => {
      const next = applyRunningGameEvent(
        state.runningGameIds,
        state.runningGamePids,
        id,
        running,
        pid,
      );
      if (!next.changed) return state;
      return {
        runningGameIds: next.ids,
        runningGamePids: next.pids,
        runningStateVersion: state.runningStateVersion + 1,
      };
    }),
  seedRunningGameIdsIfCurrent: (ids, requestVersion) => {
    let applied = false;
    set((state) => {
      if (!runningSnapshotIsCurrent(requestVersion, state.runningStateVersion)) {
        return state;
      }
      applied = true;
      const pids: Record<string, number> = {};
      for (const id of ids) {
        const pid = state.runningGamePids[id];
        if (pid != null) pids[id] = pid;
      }
      return {
        runningGameIds: [...ids],
        runningGamePids: pids,
        runningStateVersion: state.runningStateVersion + 1,
      };
    });
    return applied;
  },
}));

export function useGameEvents() {
  const {
    applyInstallProgress,
    clearInstall,
    setRunningGameId,
    seedRunningGameIdsIfCurrent,
  } = useEventStore();
  const addLog = useLogStore((s) => s.addLog);
  const setWindowFocused = useFocusStore((s) => s.setWindowFocused);
  const setMinimized = useFocusStore((s) => s.setMinimized);
  const lastWindowAction = useRef<AfterLaunchWindowAction>('none');

  useEffect(() => {
    // Race-safe event subscription. `listen()` is async — the returned
    // Promise can resolve AFTER the component has unmounted, so we cannot
    // rely on the cleanup function to invoke late unlisteners. We track a
    // `cancelled` flag and, if a `listen()` resolves after unmount, we
    // immediately call its unlistener and drop it. This avoids both
    // memory leaks and double-handlers in React 18 StrictMode where the
    // effect runs twice in development.
    let cancelled = false;
    const unlisteners: Array<() => void> = [];

    // Seed the running-instance list from the backend so the "Running"
    // badges survive a frontend reload while a game is still up.
    (async () => {
      try {
        const requestVersion = useEventStore.getState().runningStateVersion;
        const ids = await invoke<string[]>('cmd_get_launch_state');
        if (!cancelled) {
          const applied = useEventStore
            .getState()
            .seedRunningGameIdsIfCurrent(ids, requestVersion);
          if (applied) {
            useFocusStore.getState().setRunningCount(ids.length);
          }
        }
      } catch { /* ignore */ }
    })();

    const register = (
      promise: Promise<() => void>,
      onError?: (e: unknown) => void,
    ) => {
      promise
        .then((fn) => {
          if (cancelled) {
            // Effect was already torn down — detach the listener right
            // away so we don't leak a callback into a dead component.
            try { fn(); } catch { /* ignore */ }
            return;
          }
          unlisteners.push(fn);
        })
        .catch((e) => { if (onError) onError(e); });
    };

    register(listen<InstallProgressEvent>('install_progress', (event) => {
      const p = event.payload;
      applyInstallProgress(p);
      if (isTerminalInstallStage(p.stage)) {
        // Keep the terminal snapshot briefly for the overlay/hide transition,
        // then remove it so stale events cannot resurrect a finished install.
        const installId = p.install_id;
        setTimeout(() => clearInstall(installId), 1500);
      }
    }));

    register(listen<LaunchEvent>('game_started', async (event) => {
      const p = event.payload;
      if (p.status === 'running') {
        setRunningGameId(p.instance_id, true, p.pid);
        useFocusStore
          .getState()
          .setRunningCount(useEventStore.getState().runningGameIds.length);
        // Apply the configured after-launch action only once the game process
        // is actually running. A failed launch never reaches this point, so
        // the window stays visible with the error. Settings may not have been
        // opened yet in this session, so read the config directly if needed.
        let config = useSettingsStore.getState().config;
        if (!config) {
          try {
            config = await invoke<AppConfig>('cmd_get_config');
          } catch {
            config = null;
          }
        }
        const action = afterLaunchWindowAction(
          p.status,
          config?.after_launch_action,
        );
        lastWindowAction.current = action;
        if (action !== 'none') {
          try {
            const { getCurrentWindow } = await import('@tauri-apps/api/window');
            await applyAfterLaunchAction(action, getCurrentWindow());
          } catch { /* best-effort */ }
        }
      }
    }));

    register(listen<LaunchEvent>('launch_complete', async (event) => {
      const p = event.payload;
      setRunningGameId(p.instance_id, false, p.pid);
      useFocusStore
        .getState()
        .setRunningCount(useEventStore.getState().runningGameIds.length);
      // If the launcher minimized itself when the game started, restore it on
      // a crash so the user actually sees the error. "Do nothing" must not
      // steal focus, and "close" means the launcher is already gone.
      // A non-zero exit code means the Java process died — surface it in the
      // launcher log even when the window action was "do nothing".
      if (p.exit_code !== null && p.exit_code !== undefined && p.exit_code !== 0) {
        if (lastWindowAction.current === 'minimize') {
          try {
            const { getCurrentWindow } = await import('@tauri-apps/api/window');
            await getCurrentWindow().unminimize();
            await getCurrentWindow().setFocus();
          } catch { /* best-effort */ }
        }
        addLog({
          level: 'error',
          source: 'launch',
          message: `Minecraft exited unexpectedly with code ${p.exit_code}. Check the log panel for details.`,
        });
        // Crash analyzer: classify the failure right after the process
        // dies and summarize the likely cause in the log panel. The
        // backend resolves the newest session of this instance itself.
        invoke<CrashAnalysis>('cmd_analyze_crash', { instanceName: p.instance_id })
          .then((a) => {
            if (a.source === 'none' || a.source === 'clean') return;
            const entry = CRASH_KEYS[a.kind] ?? CRASH_KEYS.unknown;
            const cause = t(entry.message, { culprit: a.culprit ?? '—' });
            addLog({
              level: 'warn',
              source: 'launch',
              message: t('diag.crash_summary', { cause }),
            });
          })
          .catch(() => { /* analysis is best-effort */ });
      }
    }));

    register(listen<LogPayload>('log_message', (event) => {
      const p = event.payload;
      const level = (p.level === 'warning' ? 'warn' : p.level) as LogEntry['level'];
      addLog({ level, source: p.source, message: p.message });
    }));

    register(
      listen<{ kind: string; min_version?: string | null }>('launch_warning', (event) => {
        if (event.payload.kind === 'elyby_skins_unsupported_mc') {
          addToast(
            t('errors.elyby_skins_unsupported_mc', {
              version: event.payload.min_version ?? '1.7.2',
            }),
            'warning',
          );
        } else if (event.payload.kind === 'elyby_skins_bypassed') {
          addToast(t('errors.elyby_skins_bypassed'), 'warning');
        }
      }),
    );

    // Window focus tracking via Tauri events.
    // These fire on the webview window when OS focus or visibility changes.
    // Focus additionally RESYNCS the running-game set from the backend: a
    // lost `game_started`/`launch_complete` event (crashed game, killed
    // process, missed emit during suspend) would otherwise leave a "Running"
    // badge stuck forever. The backend reconciles persisted pid files with
    // the live process table, so it is always the source of truth.
    const resyncRunningState = async () => {
      try {
        const requestVersion = useEventStore.getState().runningStateVersion;
        const ids = await invoke<string[]>('cmd_get_launch_state');
        if (!cancelled) {
          const applied = useEventStore
            .getState()
            .seedRunningGameIdsIfCurrent(ids, requestVersion);
          if (applied) {
            useFocusStore.getState().setRunningCount(ids.length);
          }
        }
      } catch { /* ignore */ }
    };
    register(listen('tauri://focus', () => {
      setWindowFocused(true);
      void resyncRunningState();
    }));
    register(listen('tauri://blur', () => setWindowFocused(false)));

    // Minimize/restore are not exposed as their own Tauri events on Windows,
    // but both emit `tauri://resize` (the minimized window lands off-screen
    // at -32000). Ask the window itself instead of guessing from geometry.
    const syncMinimized = async () => {
      try {
        const { getCurrentWindow } = await import('@tauri-apps/api/window');
        const minimized = await getCurrentWindow().isMinimized();
        if (!cancelled) setMinimized(minimized);
      } catch { /* ignore */ }
    };
    register(listen('tauri://resize', () => { void syncMinimized(); }));
    void syncMinimized();

    return () => {
      cancelled = true;
      // Snapshot the array so any late-registered listener (a `.then`
      // that fires between this loop and the `cancelled = true` write)
      // doesn't see a half-cleared list. Items that arrive after this
      // point are caught by the `cancelled` check in `register` above.
      const snapshot = unlisteners.slice();
      unlisteners.length = 0;
      for (const fn of snapshot) {
        try { fn(); } catch { /* ignore */ }
      }
    };
  }, [applyInstallProgress, clearInstall, setRunningGameId, seedRunningGameIdsIfCurrent, addLog, setWindowFocused, setMinimized]);
}

export function emitFrontendLog(level: string, source: string, message: string) {
  invoke('cmd_emit_log', { level, source, message }).catch(() => {});
}
