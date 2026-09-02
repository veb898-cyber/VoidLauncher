import { create } from 'zustand';

interface FocusState {
  /// True when the launcher window has OS focus (set by listening to tauri://focus/blur)
  isWindowFocused: boolean;
  /// True when at least one game is running (set by listening to game_started/launch_complete)
  isGameRunning: boolean;
  /// True when window is unfocused AND a game is running — UI should freeze heavy work
  isFrozen: boolean;
  /// Internal count of concurrently running games.
  _gameCount: number;
  setWindowFocused: (focused: boolean) => void;
  /// Set the running-game count directly (used to seed from backend state).
  setRunningCount: (count: number) => void;
}

export const useFocusStore = create<FocusState>((set) => ({
  isWindowFocused: true,
  isGameRunning: false,
  isFrozen: false,
  _gameCount: 0,
  setWindowFocused: (focused) =>
    set((state) => ({
      isWindowFocused: focused,
      isFrozen: state.isGameRunning && !focused,
    })),
  setRunningCount: (count) =>
    set((state) => {
      const safe = Math.max(0, count);
      const isGameRunning = safe > 0;
      return {
        _gameCount: safe,
        isGameRunning,
        isFrozen: isGameRunning && !state.isWindowFocused,
      };
    }),
}));

/**
 * Adjust the running-game count by a delta (+1 start, -1 end). Never drops
 * below zero. Kept in this file so both the event hook and any UI can bump it.
 */
export function adjustRunningCount(delta: number) {
  const state = useFocusStore.getState();
  const next = Math.max(0, state._gameCount + delta);
  state.setRunningCount(next);
}
