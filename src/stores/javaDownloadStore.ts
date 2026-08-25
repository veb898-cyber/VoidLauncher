import { create } from 'zustand';

export interface JavaDownloadActive {
  majorVersion: number;
  percent: number;
  message: string;
}

interface JavaDownloadState {
  active: JavaDownloadActive | null;
  /** Mark a download as started from this UI (Settings button click). */
  startDownload: (majorVersion: number) => void;
  reportProgress: (majorVersion: number, percent: number, message: string) => void;
  clear: () => void;
}

/**
 * Global java-download progress. Lives outside the Settings page so the
 * progress banner survives switching tabs while a runtime downloads
 * in the background.
 */
export const useJavaDownloadStore = create<JavaDownloadState>((set) => ({
  active: null,
  startDownload: (majorVersion) =>
    set({ active: { majorVersion, percent: 0, message: 'Starting...' } }),
  reportProgress: (majorVersion, percent, message) =>
    set({ active: { majorVersion, percent, message } }),
  clear: () => set({ active: null }),
}));
