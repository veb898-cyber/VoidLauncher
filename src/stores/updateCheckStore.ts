import { create } from 'zustand';

export interface UpdateInfo {
  name: string;
  oldVersion: string;
  newVersion: string;
  downloadUrl: string;
  filename: string;
  oldFilename: string;
  projectId: string;
  versionId: string;
  expectedSha1: string;
  /** `modrinth` (default) or `curseforge` — decides the download path. */
  provider: string;
  /** CurseForge file id of the new file (id-chain download). */
  fileId?: string | null;
}

interface UpdateCheckEntry {
  running: boolean;
  updates: UpdateInfo[] | null;
}

interface UpdateCheckState {
  checks: Record<string, UpdateCheckEntry>;
  start: (key: string) => void;
  succeed: (key: string, updates: UpdateInfo[] | null) => void;
  fail: (key: string) => void;
  remove: (key: string, oldFilename: string) => void;
  dismiss: (key: string) => void;
}

export const useUpdateCheckStore = create<UpdateCheckState>((set) => ({
  checks: {},
  start: (key) => set((state) => ({
    checks: { ...state.checks, [key]: { running: true, updates: null } },
  })),
  succeed: (key, updates) => set((state) => ({
    checks: {
      ...state.checks,
      [key]: { running: false, updates: updates && updates.length > 0 ? updates : null },
    },
  })),
  fail: (key) => set((state) => ({
    checks: { ...state.checks, [key]: { running: false, updates: null } },
  })),
  remove: (key, oldFilename) => set((state) => {
    const entry = state.checks[key];
    if (!entry?.updates) return state;
    const next = entry.updates.filter((u) => u.oldFilename !== oldFilename);
    return {
      checks: { ...state.checks, [key]: { ...entry, updates: next.length > 0 ? next : null } },
    };
  }),
  dismiss: (key) => set((state) => ({
    checks: { ...state.checks, [key]: { running: false, updates: null } },
  })),
}));

export const EMPTY_UPDATE_CHECK: UpdateCheckEntry = { running: false, updates: null };
