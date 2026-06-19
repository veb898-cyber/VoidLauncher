import { create } from 'zustand';

interface GameLogState {
  selectedPath: string | null;
  setSelectedPath: (path: string | null) => void;
}

export const useGameLogStore = create<GameLogState>((set) => ({
  selectedPath: null,
  setSelectedPath: (path) => set({ selectedPath: path }),
}));
