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

export const useLogStore = create<LogState>((set) => ({
  logs: [],
  maxLogs: 1000,

  addLog: (entry) =>
    set((state) => {
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
