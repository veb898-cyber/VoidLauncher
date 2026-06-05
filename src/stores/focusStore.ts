import { create } from 'zustand';

interface FocusState {
  /// True when the launcher window has OS focus (set by listening to tauri://focus/blur)
  isWindowFocused: boolean;
  /// True when a game is currently running (set by listening to game_started/launch_complete)
  isGameRunning: boolean;
  /// True when window is unfocused AND a game is running — UI should freeze heavy work
  isFrozen: boolean;
  setWindowFocused: (focused: boolean) => void;
  setGameRunning: (running: boolean) => void;
}

export const useFocusStore = create<FocusState>((set) => ({
  isWindowFocused: true,
  isGameRunning: false,
  isFrozen: false,
  setWindowFocused: (focused) =>
    set((state) => ({
      isWindowFocused: focused,
      isFrozen: state.isGameRunning && !focused,
    })),
  setGameRunning: (running) =>
    set((state) => ({
      isGameRunning: running,
      isFrozen: running && !state.isWindowFocused,
    })),
}));
