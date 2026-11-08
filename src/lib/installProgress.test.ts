import { describe, expect, it } from 'vitest';
import {
  applyInstallProgress,
  beginInstall,
  clearInstall,
  createInstallId,
  EMPTY_INSTALL_PROGRESS_STATE,
  installDisplay,
  isTerminalInstallStage,
  type InstallProgressEvent,
} from './installProgress';

function event(overrides: Partial<InstallProgressEvent> = {}): InstallProgressEvent {
  return {
    install_id: 'install-A',
    instance_id: 'A',
    percent: 20,
    prepared_bytes: 20,
    in_progress_bytes: 0,
    network_downloaded_bytes: 20,
    total_bytes: 100,
    indeterminate: false,
    stage: 'libraries',
    message: 'Downloading libraries...',
    ...overrides,
  };
}

describe('install progress identity', () => {
  it('ignores events from a foreign install_id', () => {
    const state = beginInstall(EMPTY_INSTALL_PROGRESS_STATE, 'install-B');
    const next = applyInstallProgress(state, event({ install_id: 'install-A' }));
    expect(next).toBe(state);
    expect(next.installProgress).toBeNull();
  });

  it('does not mix two simultaneous installs', () => {
    let state = beginInstall(EMPTY_INSTALL_PROGRESS_STATE, 'install-A');
    state = applyInstallProgress(state, event({ install_id: 'install-A', prepared_bytes: 20 }));

    state = beginInstall(state, 'install-B');
    state = applyInstallProgress(state, event({ install_id: 'install-A', prepared_bytes: 90 }));
    expect(state.installProgress).toBeNull();

    state = applyInstallProgress(
      state,
      event({ install_id: 'install-B', instance_id: 'B', prepared_bytes: 10 }),
    );
    expect(state.installProgress?.install_id).toBe('install-B');
    expect(state.installProgress?.prepared_bytes).toBe(10);
  });

  it('ignores late events from a finished install', () => {
    let state = beginInstall(EMPTY_INSTALL_PROGRESS_STATE, 'install-A');
    state = applyInstallProgress(state, event());
    state = applyInstallProgress(state, event({ stage: 'done', percent: 100 }));
    expect(state.activeInstallId).toBeNull();
    const finished = state;

    state = applyInstallProgress(
      state,
      event({ stage: 'assets', prepared_bytes: 200, total_bytes: 300 }),
    );
    expect(state).toBe(finished);
  });

  it('keeps cumulative bytes across a stage change', () => {
    const mb = 1024 * 1024;
    let state = beginInstall(EMPTY_INSTALL_PROGRESS_STATE, 'install-A');
    state = applyInstallProgress(
      state,
      event({
        stage: 'libraries',
        prepared_bytes: 50 * mb,
        in_progress_bytes: 10 * mb,
        network_downloaded_bytes: 20 * mb,
        total_bytes: 100 * mb,
        percent: 60,
      }),
    );
    state = applyInstallProgress(
      state,
      event({
        stage: 'assets',
        prepared_bytes: 120 * mb,
        in_progress_bytes: 5 * mb,
        network_downloaded_bytes: 60 * mb,
        total_bytes: 300 * mb,
        percent: 41.66,
      }),
    );
    expect(state.installProgress?.prepared_bytes).toBe(120 * mb);
    expect(state.installProgress?.in_progress_bytes).toBe(5 * mb);
    expect(state.installProgress?.network_downloaded_bytes).toBe(60 * mb);
    expect(state.installProgress?.total_bytes).toBe(300 * mb);
    const display = installDisplay(state.installProgress!);
    expect(display.preparedMb).toBe('120.0');
    expect(display.inProgressMb).toBe('5.0');
    expect(display.totalMb).toBe('300.0');
    expect(display.showBytes).toBe(true);
  });

  it('never moves the cumulative network counter backwards on out-of-order events', () => {
    let state = beginInstall(EMPTY_INSTALL_PROGRESS_STATE, 'install-A');
    state = applyInstallProgress(state, event({ network_downloaded_bytes: 70 }));
    state = applyInstallProgress(state, event({ network_downloaded_bytes: 30 }));
    expect(state.installProgress?.network_downloaded_bytes).toBe(70);
  });

  it('lets partial bytes drop on retry without touching prepared bytes', () => {
    let state = beginInstall(EMPTY_INSTALL_PROGRESS_STATE, 'install-A');
    state = applyInstallProgress(
      state,
      event({
        prepared_bytes: 50,
        in_progress_bytes: 70,
        network_downloaded_bytes: 70,
        percent: 90,
      }),
    );
    state = applyInstallProgress(
      state,
      event({
        prepared_bytes: 50,
        in_progress_bytes: 10,
        network_downloaded_bytes: 10,
        percent: 30,
      }),
    );
    expect(state.installProgress?.prepared_bytes).toBe(50);
    expect(state.installProgress?.in_progress_bytes).toBe(10);
    expect(state.installProgress?.network_downloaded_bytes).toBe(70);
    // The backend legitimately lowers the percentage when partial bytes are
    // discarded; only `done` may reach 100%.
    expect(state.installProgress?.percent).toBe(30);
  });

  it('keeps a lower determinate percentage after replanning', () => {
    let state = beginInstall(EMPTY_INSTALL_PROGRESS_STATE, 'install-A');
    state = applyInstallProgress(state, event({ percent: 40, indeterminate: false }));
    state = applyInstallProgress(state, event({ percent: 30, indeterminate: false }));
    expect(state.installProgress?.percent).toBe(30);

    state = applyInstallProgress(
      state,
      event({ percent: 5, indeterminate: true, total_bytes: 0 }),
    );
    expect(state.installProgress?.percent).toBe(5);
    state = applyInstallProgress(
      state,
      event({ percent: 15, indeterminate: false, total_bytes: 200 }),
    );
    expect(state.installProgress?.percent).toBe(15);
  });

  it('clears only the matching install state', () => {
    let state = beginInstall(EMPTY_INSTALL_PROGRESS_STATE, 'install-A');
    state = applyInstallProgress(state, event());
    expect(clearInstall(state, 'install-B')).toBe(state);
    const cleared = clearInstall(state, 'install-A');
    expect(cleared.installProgress).toBeNull();
    expect(cleared.activeInstallId).toBeNull();
  });

  it('clears a started operation even before its first event arrived', () => {
    const state = beginInstall(EMPTY_INSTALL_PROGRESS_STATE, 'install-A');
    const cleared = clearInstall(state, 'install-A');
    expect(cleared.activeInstallId).toBeNull();
    expect(cleared.installProgress).toBeNull();
  });

  it('creates a non-empty unique install id', () => {
    const one = createInstallId();
    const two = createInstallId();
    expect(one.length).toBeGreaterThan(0);
    expect(two.length).toBeGreaterThan(0);
    expect(one).not.toBe(two);
  });
});

describe('install progress display', () => {
  it('shows indeterminate without a fake percentage for unknown totals', () => {
    const display = installDisplay(
      event({
        indeterminate: true,
        total_bytes: 0,
        prepared_bytes: 12,
        in_progress_bytes: 30,
        percent: 0,
      }),
    );
    expect(display.indeterminate).toBe(true);
    expect(display.showPercent).toBe(false);
    expect(display.showBytes).toBe(false);
    expect(display.showInProgress).toBe(false);
  });

  it('shows prepared megabytes against the known total', () => {
    const display = installDisplay(
      event({
        prepared_bytes: 10 * 1024 * 1024,
        in_progress_bytes: 0,
        total_bytes: 40 * 1024 * 1024,
        percent: 25,
      }),
    );
    expect(display.showBytes).toBe(true);
    expect(display.preparedMb).toBe('10.0');
    expect(display.inProgressMb).toBe('0.0');
    expect(display.showInProgress).toBe(false);
    expect(display.totalMb).toBe('40.0');
    expect(display.percent).toBe(25);
  });

  it('shows in-progress bytes separately from prepared bytes', () => {
    const display = installDisplay(
      event({
        prepared_bytes: 10 * 1024 * 1024,
        in_progress_bytes: 30 * 1024 * 1024,
        total_bytes: 40 * 1024 * 1024,
        percent: 99,
      }),
    );
    expect(display.preparedMb).toBe('10.0');
    expect(display.inProgressMb).toBe('30.0');
    expect(display.showInProgress).toBe(true);
    expect(display.percent).toBe(99);
  });

  it('does not report partial download bytes as prepared', () => {
    const display = installDisplay(
      event({
        prepared_bytes: 0,
        in_progress_bytes: 40 * 1024 * 1024,
        network_downloaded_bytes: 0,
        total_bytes: 40 * 1024 * 1024,
        percent: 99,
      }),
    );
    expect(display.preparedMb).toBe('0.0');
    expect(display.inProgressMb).toBe('40.0');
    expect(display.showInProgress).toBe(true);
    expect(display.showNetworkBytes).toBe(false);
  });

  it('shows network bytes separately when there were network transfers', () => {
    const display = installDisplay(
      event({
        prepared_bytes: 30 * 1024 * 1024,
        in_progress_bytes: 0,
        network_downloaded_bytes: 8 * 1024 * 1024,
        total_bytes: 40 * 1024 * 1024,
        percent: 75,
      }),
    );
    expect(display.preparedMb).toBe('30.0');
    expect(display.networkMb).toBe('8.0');
    expect(display.showNetworkBytes).toBe(true);
  });

  it('does not present cached files as network downloads or in progress', () => {
    const display = installDisplay(
      event({
        prepared_bytes: 40 * 1024 * 1024,
        in_progress_bytes: 0,
        network_downloaded_bytes: 0,
        total_bytes: 40 * 1024 * 1024,
        percent: 100,
      }),
    );
    expect(display.preparedMb).toBe('40.0');
    expect(display.inProgressMb).toBe('0.0');
    expect(display.networkMb).toBe('0.0');
    expect(display.showInProgress).toBe(false);
    expect(display.showNetworkBytes).toBe(false);
  });

  it('does not report 100 percent before done', () => {
    const display = installDisplay(
      event({ stage: 'assets', percent: 100, prepared_bytes: 100, total_bytes: 100 }),
    );
    expect(display.percent).toBe(99);
  });

  it('reports 100 percent only for done', () => {
    const display = installDisplay(
      event({ stage: 'done', percent: 100, prepared_bytes: 100, total_bytes: 100 }),
    );
    expect(display.percent).toBe(100);
    expect(isTerminalInstallStage('done')).toBe(true);
  });

  it('treats errors/cancellation as terminal and never as complete', () => {
    expect(isTerminalInstallStage('error')).toBe(true);
    expect(isTerminalInstallStage('cancelled')).toBe(true);
    const display = installDisplay(
      event({ stage: 'error', percent: 100, prepared_bytes: 0, in_progress_bytes: 30 }),
    );
    expect(display.indeterminate).toBe(true);
    expect(display.percent).toBe(0);
    expect(display.preparedMb).toBe('0.0');
    expect(display.showInProgress).toBe(false);
  });
});
