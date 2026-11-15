import { create } from 'zustand';

export interface JavaDownloadActive {
  majorVersion: number;
  percent: number;
  stage: string;
  message: string;
}

interface JavaDownloadState {
  /**
   * One entry per Java major. A map (not a single slot) is essential: two
   * launches can need Java 17 and Java 21 at the same time, and their progress
   * events must never overwrite each other.
   */
  active: Record<number, JavaDownloadActive>;
  /** Mark a download as started from this UI (Settings button click). */
  startDownload: (majorVersion: number) => void;
  reportProgress: (
    majorVersion: number,
    percent: number,
    message: string,
    stage: string,
  ) => void;
  /** Remove one major, or every entry when `majorVersion` is omitted. */
  clear: (majorVersion?: number) => void;
}

/**
 * Global java-download progress. Lives outside the Settings page so the
 * progress banner survives switching tabs while a runtime downloads
 * in the background.
 */
export const useJavaDownloadStore = create<JavaDownloadState>((set) => ({
  active: {},
  startDownload: (majorVersion) =>
    set((state) => ({
      active: {
        ...state.active,
        [majorVersion]: {
          majorVersion,
          percent: 0,
          stage: 'resolving',
          message: 'Starting...',
        },
      },
    })),
  reportProgress: (majorVersion, percent, message, stage) =>
    set((state) => ({
      active: {
        ...state.active,
        [majorVersion]: { majorVersion, percent, message, stage },
      },
    })),
  clear: (majorVersion) =>
    set((state) => {
      if (majorVersion === undefined) return { active: {} };
      const next = { ...state.active };
      delete next[majorVersion];
      return { active: next };
    }),
}));
