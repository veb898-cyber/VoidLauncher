export type CacheClearPhase = 'idle' | 'clearing' | 'error' | 'success';

export interface CacheClearState {
  phase: CacheClearPhase;
  error: string | null;
  freedBytes: number | null;
}

export const initialCacheClearState: CacheClearState = {
  phase: 'idle',
  error: null,
  freedBytes: null,
};

/**
 * Starts a clear operation. While one is already running the state is returned
 * unchanged, so a double click or a second handler cannot start a parallel
 * deletion. Re-entering from `error` starts a retry and drops the old error.
 */
export function beginCacheClear(state: CacheClearState): CacheClearState {
  if (state.phase === 'clearing') return state;
  return { phase: 'clearing', error: null, freedBytes: null };
}

/** Records a failed clear; only a running operation can fail. */
export function failCacheClear(state: CacheClearState, error: string): CacheClearState {
  if (state.phase !== 'clearing') return state;
  return { phase: 'error', error, freedBytes: null };
}

/** Marks a successful clear and keeps the freed byte count for the result. */
export function finishCacheClear(state: CacheClearState, freedBytes: number): CacheClearState {
  if (state.phase !== 'clearing') return state;
  return { phase: 'success', error: null, freedBytes };
}

/** The dialog must stay open while files are being deleted. */
export function canDismissCacheClear(state: CacheClearState): boolean {
  return state.phase !== 'clearing';
}
