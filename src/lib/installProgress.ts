/**
 * Install-progress identity and display rules.
 *
 * Backend events are cumulative across stages and carry a unique install_id.
 * The UI must never merge two installs, must ignore late events from a
 * finished install, and must not invent a percentage while the total size is
 * unknown.
 */

export interface InstallProgressEvent {
  install_id: string;
  instance_id: string;
  percent: number;
  /** Fully verified bytes: valid cache plus files downloaded and verified in this operation. */
  prepared_bytes: number;
  /** Partial bytes of files still downloading, not yet verified. */
  in_progress_bytes: number;
  /** Bytes actually transferred over the network in this operation (incl. retries). */
  network_downloaded_bytes: number;
  total_bytes: number;
  indeterminate: boolean;
  stage: string;
  message: string;
}

export const TERMINAL_INSTALL_STAGES = ['done', 'error', 'cancelled'] as const;

export function isTerminalInstallStage(stage: string): boolean {
  return (TERMINAL_INSTALL_STAGES as readonly string[]).includes(stage);
}

export function createInstallId(): string {
  try {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
      return crypto.randomUUID();
    }
  } catch {
    // fall through to the timestamp fallback
  }
  return `install-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export interface InstallProgressState {
  activeInstallId: string | null;
  installProgress: InstallProgressEvent | null;
  finishedInstallIds: string[];
}

export const EMPTY_INSTALL_PROGRESS_STATE: InstallProgressState = {
  activeInstallId: null,
  installProgress: null,
  finishedInstallIds: [],
};

/** Start (or switch to) one operation. A new install never inherits old bytes. */
export function beginInstall(
  state: InstallProgressState,
  installId: string,
): InstallProgressState {
  return {
    ...state,
    activeInstallId: installId,
    installProgress: null,
  };
}

/** Accept only events belonging to the active operation. */
export function applyInstallProgress(
  state: InstallProgressState,
  event: InstallProgressEvent,
): InstallProgressState {
  if (state.finishedInstallIds.includes(event.install_id)) return state;
  if (state.activeInstallId === null) return state;
  if (event.install_id !== state.activeInstallId) return state;

  const previous = state.installProgress;
  const sameInstall = previous?.install_id === event.install_id;
  const progress: InstallProgressEvent = {
    ...event,
    // Prepared and in-progress bytes are the current contributions of each
    // file: a retry may legitimately lower both (the partial file was
    // discarded). Network bytes are cumulative per operation and are clamped
    // against out-of-order delivery.
    prepared_bytes: event.prepared_bytes,
    in_progress_bytes: event.in_progress_bytes,
    network_downloaded_bytes: sameInstall
      ? Math.max(previous.network_downloaded_bytes, event.network_downloaded_bytes)
      : event.network_downloaded_bytes,
    // Follow the backend percentage: it is computed from
    // prepared + in-progress, so a retry that discards partial bytes may
    // legitimately lower it. Only `done` may raise it to 100.
    percent: event.percent,
  };

  if (isTerminalInstallStage(event.stage)) {
    const finished = [
      event.install_id,
      ...state.finishedInstallIds.filter((id) => id !== event.install_id),
    ].slice(0, 20);
    return {
      activeInstallId: null,
      installProgress: progress,
      finishedInstallIds: finished,
    };
  }

  return { ...state, installProgress: progress };
}

/** Remove the transient overlay state for a finished install. */
export function clearInstall(
  state: InstallProgressState,
  installId: string,
): InstallProgressState {
  if (
    state.activeInstallId !== installId &&
    state.installProgress?.install_id !== installId
  ) {
    return state;
  }
  return {
    ...state,
    activeInstallId: state.activeInstallId === installId ? null : state.activeInstallId,
    installProgress: null,
  };
}

export interface InstallDisplay {
  indeterminate: boolean;
  showPercent: boolean;
  percent: number;
  showBytes: boolean;
  preparedMb: string;
  inProgressMb: string;
  totalMb: string;
  networkMb: string;
  showInProgress: boolean;
  showNetworkBytes: boolean;
}

export function installDisplay(progress: InstallProgressEvent): InstallDisplay {
  const indeterminate =
    progress.indeterminate || progress.total_bytes <= 0 || progress.stage === 'error';
  const percent = Math.max(
    0,
    Math.min(
      progress.stage === 'done' ? 100 : 99,
      indeterminate ? 0 : progress.percent,
    ),
  );
  const showBytes = !indeterminate && progress.total_bytes > 0;
  return {
    indeterminate,
    showPercent: !indeterminate,
    percent,
    showBytes,
    preparedMb: (progress.prepared_bytes / 1024 / 1024).toFixed(1),
    inProgressMb: (progress.in_progress_bytes / 1024 / 1024).toFixed(1),
    totalMb: (progress.total_bytes / 1024 / 1024).toFixed(1),
    networkMb: (progress.network_downloaded_bytes / 1024 / 1024).toFixed(1),
    // Partial bytes are shown separately, never as prepared.
    showInProgress: showBytes && progress.in_progress_bytes > 0,
    // Cached-only operations must not look like network traffic.
    showNetworkBytes: showBytes && progress.network_downloaded_bytes > 0,
  };
}
