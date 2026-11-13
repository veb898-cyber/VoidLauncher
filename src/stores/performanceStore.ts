import { create } from 'zustand';
import {
  migrateLegacyPerformanceSettings,
  writeReduceInterfaceEffects,
} from '../lib/performanceRuntime';

interface PerformanceState {
  /** Opt-in reduction of non-essential interface effects. Off by default. */
  reduceInterfaceEffects: boolean;
  setReduceInterfaceEffects: (enabled: boolean) => void;
}

/**
 * The attribute is applied directly to <html> instead of through React so
 * switching the option never re-renders the application tree. Components
 * that really need the value subscribe to this tiny store.
 */
function applyReduceInterfaceEffects(enabled: boolean): void {
  const root = document.documentElement;
  if (enabled) root.setAttribute('data-reduce-effects', 'on');
  else root.removeAttribute('data-reduce-effects');
}

const initial = migrateLegacyPerformanceSettings();
applyReduceInterfaceEffects(initial);

export const usePerformanceStore = create<PerformanceState>((set) => ({
  reduceInterfaceEffects: initial,

  setReduceInterfaceEffects: (enabled) => {
    set({ reduceInterfaceEffects: enabled });
    writeReduceInterfaceEffects(enabled);
    applyReduceInterfaceEffects(enabled);
  },
}));
