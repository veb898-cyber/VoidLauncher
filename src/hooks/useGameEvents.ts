import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import { useLogStore } from '../stores/logStore';
import type { LogEntry } from '../stores/logStore';
import { useFocusStore, adjustRunningCount } from '../stores/focusStore';

interface InstallProgress {
  instance_id: string;
  percent: number;
  downloaded_bytes: number;
  total_bytes: number;
  stage: string;
  message: string;
}

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
  installProgress: InstallProgress | null;
  /** Names of the instances whose games are currently running (may be several). */
  runningGameIds: string[];
  setInstallProgress: (p: InstallProgress | null) => void;
  /** Add or remove a single instance from the running set. */
  setRunningGameId: (id: string, running: boolean) => void;
  /** Replace the whole running set (used when seeding from backend state). */
  seedRunningGameIds: (ids: string[]) => void;
}

export const useEventStore = create<EventState>((set) => ({
  installProgress: null,
  runningGameIds: [],
  setInstallProgress: (p) => set({ installProgress: p }),
  setRunningGameId: (id, running) =>
    set((state) => {
      if (!running) {
        return { runningGameIds: state.runningGameIds.filter((g) => g !== id) };
      }
      return state.runningGameIds.includes(id)
        ? state
        : { runningGameIds: [...state.runningGameIds, id] };
    }),
  seedRunningGameIds: (ids) => set({ runningGameIds: [...ids] }),
}));

export function useGameEvents() {
  const { setInstallProgress, setRunningGameId, seedRunningGameIds } = useEventStore();
  const addLog = useLogStore((s) => s.addLog);
  const setWindowFocused = useFocusStore((s) => s.setWindowFocused);

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
        const ids = await invoke<string[]>('cmd_get_launch_state');
        if (!cancelled) {
          seedRunningGameIds(ids);
          useFocusStore.getState().setRunningCount(ids.length);
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

    register(listen<InstallProgress>('install_progress', (event) => {
      const p = event.payload;
      setInstallProgress(p);
      if (p.stage === 'done') {
        setTimeout(() => setInstallProgress(null), 2000);
      }
    }));

    register(listen<LaunchEvent>('game_started', async (event) => {
      const p = event.payload;
      if (p.status === 'running') {
        setRunningGameId(p.instance_id, true);
        adjustRunningCount(1);
        // Minimize the launcher only once the game process is actually
        // running. If the launch fails before this point, the window stays
        // visible so the user sees the error.
        try {
          const { getCurrentWindow } = await import('@tauri-apps/api/window');
          await getCurrentWindow().minimize();
        } catch { /* best-effort */ }
      }
    }));

    register(listen<LaunchEvent>('launch_complete', async (event) => {
      const p = event.payload;
      setRunningGameId(p.instance_id, false);
      adjustRunningCount(-1);
      // If the launcher minimized itself when the game started (see the
      // `game_started` handler), restore it on a crash so the user actually
      // sees the error. A non-zero exit code means the Java process died —
      // surface it in the launcher log so users can find the cause without
      // having to dig through Minecraft's own logs.
      if (p.exit_code !== null && p.exit_code !== undefined && p.exit_code !== 0) {
        try {
          const { getCurrentWindow } = await import('@tauri-apps/api/window');
          await getCurrentWindow().unminimize();
          await getCurrentWindow().setFocus();
        } catch { /* best-effort */ }
        addLog({
          level: 'error',
          source: 'launch',
          message: `Minecraft exited unexpectedly with code ${p.exit_code}. Check the log panel for details.`,
        });
      }
    }));

    register(listen<LogPayload>('log_message', (event) => {
      const p = event.payload;
      const level = (p.level === 'warning' ? 'warn' : p.level) as LogEntry['level'];
      addLog({ level, source: p.source, message: p.message });
    }));

    // Window focus tracking via Tauri events.
    // These fire on the webview window when OS focus or visibility changes.
    register(listen('tauri://focus', () => setWindowFocused(true)));
    register(listen('tauri://blur', () => setWindowFocused(false)));
    register(listen('tauri://minimize', () => setWindowFocused(false)));
    register(listen('tauri://restore', () => setWindowFocused(true)));

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
  }, [setInstallProgress, setRunningGameId, seedRunningGameIds, addLog, setWindowFocused]);
}

export function emitFrontendLog(level: string, source: string, message: string) {
  invoke('cmd_emit_log', { level, source, message }).catch(() => {});
}
