/**
 * Pure rules shared by the Servers screen and its tests. The component keeps
 * the actual React state; these helpers make the lifecycle/race rules
 * explicit instead of burying them in effects.
 */

export type ServersViewState = 'loading' | 'error' | 'empty' | 'list';

export function serversViewState(input: {
  loading: boolean;
  hasLoaded: boolean;
  error: string | null;
  count: number;
}): ServersViewState {
  if (input.loading && !input.hasLoaded) return 'loading';
  // A read failure must never be rendered as an empty server list.
  if (input.error && !input.hasLoaded) return 'error';
  if (input.count === 0) return 'empty';
  return 'list';
}

/** Reload only on a real false -> true focus transition, not on every render. */
export function shouldReloadOnFocus(wasFocused: boolean, isFocused: boolean): boolean {
  return !wasFocused && isFocused;
}

/** The game just exited: the list may have changed inside Minecraft. */
export function shouldReloadAfterRunningChange(
  wasRunning: boolean,
  isRunning: boolean,
): boolean {
  return wasRunning && !isRunning;
}

/** Edit controls are locked only for the instance whose game is running. */
export function isServerEditingLocked(
  runningGameIds: readonly string[],
  instanceName: string,
): boolean {
  return runningGameIds.includes(instanceName);
}

/**
 * A response is usable only for the request and instance that are still
 * current. This rejects a late reply from an instance the user has already
 * switched away from.
 */
export function responseIsCurrent(input: {
  responseSeq: number;
  currentSeq: number;
  responseInstance: string;
  currentInstance: string;
}): boolean {
  return (
    input.responseSeq === input.currentSeq &&
    input.responseInstance === input.currentInstance
  );
}
