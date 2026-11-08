import { describe, expect, it } from 'vitest';
import {
  applyRunningGameEvent,
  isInstanceRunning,
  runningSnapshotIsCurrent,
  updateRunningGameIds,
} from './runningGames';

describe('instance-specific running game state', () => {
  it('does not warn for instance B while only A is running', () => {
    expect(isInstanceRunning(['A'], 'B')).toBe(false);
  });

  it('warns for instance A while A is running', () => {
    expect(isInstanceRunning(['A'], 'A')).toBe(true);
  });

  it('does not warn when no game is running', () => {
    expect(isInstanceRunning([], 'A')).toBe(false);
  });

  it('recomputes when the selected instance changes', () => {
    const running = ['A'];
    expect(isInstanceRunning(running, 'A')).toBe(true);
    expect(isInstanceRunning(running, 'B')).toBe(false);
    expect(isInstanceRunning(running, 'A')).toBe(true);
  });

  it('stops warning after the game exits', () => {
    const afterExit = updateRunningGameIds(['A'], 'A', false);
    expect(isInstanceRunning(afterExit, 'A')).toBe(false);
  });

  it('a late/independent event for A does not overwrite the state for B', () => {
    const started = updateRunningGameIds(['B'], 'A', true);
    expect(isInstanceRunning(started, 'B')).toBe(true);
    expect(isInstanceRunning(started, 'A')).toBe(true);

    const onlyB = updateRunningGameIds(['A', 'B'], 'A', false);
    expect(isInstanceRunning(onlyB, 'B')).toBe(true);
    expect(isInstanceRunning(onlyB, 'A')).toBe(false);
  });

  it('adding the same instance twice keeps one entry', () => {
    expect(updateRunningGameIds(['A'], 'A', true)).toEqual(['A']);
  });

  it('ignores a late backend snapshot that would overwrite newer events', () => {
    expect(runningSnapshotIsCurrent(3, 3)).toBe(true);
    // A game_started/launch_complete event arrived while the request was in
    // flight, so the older snapshot must be dropped.
    expect(runningSnapshotIsCurrent(3, 4)).toBe(false);
  });

  it('ignores a completion from an older run after a newer run starts', () => {
    const started = applyRunningGameEvent([], {}, 'A', true, 100);
    expect(started.changed).toBe(true);
    expect(started.ids).toEqual(['A']);
    expect(started.pids).toEqual({ A: 100 });

    const restarted = applyRunningGameEvent(started.ids, started.pids, 'A', true, 200);
    expect(restarted.pids).toEqual({ A: 200 });

    const staleComplete = applyRunningGameEvent(
      restarted.ids,
      restarted.pids,
      'A',
      false,
      100,
    );
    expect(staleComplete.changed).toBe(false);
    expect(staleComplete.ids).toEqual(['A']);
    expect(staleComplete.pids).toEqual({ A: 200 });

    const currentComplete = applyRunningGameEvent(
      restarted.ids,
      restarted.pids,
      'A',
      false,
      200,
    );
    expect(currentComplete.changed).toBe(true);
    expect(currentComplete.ids).toEqual([]);
    expect(currentComplete.pids).toEqual({});
  });

  it('does not bump the version for a duplicate start or an unknown completion', () => {
    const started = applyRunningGameEvent(['A'], { A: 100 }, 'A', true, 100);
    expect(started.changed).toBe(false);
    const unknown = applyRunningGameEvent(['A'], { A: 100 }, 'B', false, 200);
    expect(unknown.changed).toBe(false);
    expect(unknown.ids).toEqual(['A']);
  });
});
