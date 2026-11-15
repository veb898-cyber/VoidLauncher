/**
 * Decide what the content browser does after all install attempts finish.
 *
 * A successful batch returns the user straight to the installed-content list
 * (the caller awaits the list refresh before closing). Any failure keeps the
 * results screen visible so the failed cards and their reasons are not lost:
 * a Toast alone is not enough because notifications can be disabled.
 */
export type InstallCompletionAction = 'return' | 'results';

export function installCompletionAction(
  success: number,
  failed: number,
): InstallCompletionAction {
  if (failed === 0 && success > 0) return 'return';
  return 'results';
}

/** True only after every queued file was attempted (success + failed). */
export function installBatchFinished(
  attempted: number,
  total: number,
  installing: boolean,
): boolean {
  return !installing && total > 0 && attempted >= total;
}

/**
 * Refresh the installed-content list before closing the install pane. The
 * close callback runs exactly once and only after the refresh settles, so the
 * list never races the unmount and never shows a stale view. A failed refresh
 * still navigates: the directory watcher can retry in the background.
 */
export async function returnToInstalledContent(
  refresh: () => Promise<void> | void,
  close: () => void,
): Promise<void> {
  try {
    await refresh();
  } catch {
    // Navigation must not depend on a transient list-read failure.
  } finally {
    close();
  }
}
