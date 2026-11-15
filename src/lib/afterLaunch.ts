import type { AppConfig } from '../stores/settingsStore';

export type AfterLaunchAction = AppConfig['after_launch_action'];
export type AfterLaunchWindowAction = 'none' | 'minimize' | 'close';

export interface AfterLaunchWindow {
  minimize: () => Promise<void>;
  close: () => Promise<void>;
}

/**
 * Decide whether a game event may move the launcher window. Only the
 * confirmed `running` event qualifies: a failed or still-starting launch must
 * leave the window exactly where the user left it.
 */
export function afterLaunchWindowAction(
  status: string,
  action: AfterLaunchAction | string | undefined,
): AfterLaunchWindowAction {
  if (status !== 'running') return 'none';
  if (action === 'minimize' || action === 'close') return action;
  return 'none';
}

/** Apply the chosen action through the caller's window adapter. */
export async function applyAfterLaunchAction(
  action: AfterLaunchWindowAction,
  window: AfterLaunchWindow,
): Promise<AfterLaunchWindowAction> {
  if (action === 'minimize') {
    await window.minimize();
  } else if (action === 'close') {
    await window.close();
  }
  return action;
}
