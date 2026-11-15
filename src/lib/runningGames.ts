/**
 * Running-game identity rules.
 *
 * The launcher can have several Minecraft instances running at once. UI that
 * warns about game-specific reload behaviour must ask about the *selected*
 * instance, never about a global "some game is running" flag.
 */

/** Pure set update used by the event store (events may arrive out of order). */
export function updateRunningGameIds(
  ids: readonly string[],
  id: string,
  running: boolean,
): string[] {
  if (!running) return ids.filter((value) => value !== id);
  return ids.includes(id) ? [...ids] : [...ids, id];
}

export interface RunningGameSet {
  ids: string[];
  pids: Record<string, number>;
  changed: boolean;
}

/**
 * Apply one game_started/launch_complete event. A completion carrying an old
 * PID must not remove a newer run of the same instance; a completion without
 * a PID keeps the legacy behavior. A start replaces the recorded PID.
 */
export function applyRunningGameEvent(
  ids: readonly string[],
  pids: Readonly<Record<string, number>>,
  id: string,
  running: boolean,
  pid?: number | null,
): RunningGameSet {
  const currentPid = pids[id];
  if (!running && pid != null && currentPid != null && currentPid !== pid) {
    return { ids: [...ids], pids: { ...pids }, changed: false };
  }
  const wasRunning = ids.includes(id);
  const nextPids = { ...pids };
  if (running) {
    if (pid != null) nextPids[id] = pid;
  } else {
    delete nextPids[id];
  }
  const nextIds = updateRunningGameIds(ids, id, running);
  const pidChanged = running
    ? pid != null && currentPid !== pid
    : currentPid != null;
  const changed = wasRunning !== running || pidChanged;
  return { ids: nextIds, pids: nextPids, changed };
}

/** True only when this exact instance is in the running set. */
export function isInstanceRunning(ids: readonly string[], instanceName: string): boolean {
  return ids.includes(instanceName);
}

/**
 * A backend snapshot is only authoritative if no game_started/launch_complete
 * event arrived while the request was in flight. Otherwise a late response
 * for A could erase a just-started B.
 */
export function runningSnapshotIsCurrent(
  requestVersion: number,
  currentVersion: number,
): boolean {
  return requestVersion === currentVersion;
}
