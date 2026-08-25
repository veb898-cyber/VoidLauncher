import { create } from 'zustand';

export interface LogEntry {
  id: number;
  timestamp: string;
  level: 'info' | 'warn' | 'error' | 'debug';
  message: string;
  source: string;
}

interface LogState {
  logs: LogEntry[];
  maxLogs: number;
  addLog: (entry: Omit<LogEntry, 'id' | 'timestamp'>) => void;
  clearLogs: () => void;
}

let nextId = 0;

/**
 * Launcher logs are English-only by design, but UI mirrors (toasts, inline
 * banners) follow the interface language and would leak Russian into the log.
 * Such entries are dropped here; game output sources pass through untouched.
 */
const CYRILLIC_RE = /[\u0400-\u04FF]/;
function isLocalizedUiNoise(entry: { source: string; message: string }): boolean {
  return (
    entry.source !== 'minecraft' &&
    entry.source !== 'launch' &&
    CYRILLIC_RE.test(entry.message)
  );
}

export const useLogStore = create<LogState>((set) => ({
  logs: [],
  maxLogs: 1000,

  addLog: (entry) =>
    set((state) => {
      if (isLocalizedUiNoise(entry)) return state;
      const newEntry: LogEntry = {
        ...entry,
        id: nextId++,
        timestamp: new Date().toLocaleTimeString(),
      };
      const logs = [...state.logs, newEntry];
      if (logs.length > state.maxLogs) {
        return { logs: logs.slice(-state.maxLogs) };
      }
      return { logs };
    }),

  clearLogs: () => set({ logs: [] }),
}));
