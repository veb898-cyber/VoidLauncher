import type { MessageKey } from './i18n';

/** Where instance creation stopped. */
export type CreateInstanceStage = 'create' | 'game-files' | 'loader';

export interface CreateInstanceFailurePlan {
  /** i18n key for the toast shown to the user. */
  toastKey: MessageKey;
  /**
   * True when the instance directory was produced by this attempt. Retrying
   * "Create" would only hit "already exists" (and re-run the loader install),
   * so the wizard must close and the instance should be finished through
   * Verify & Repair.
   */
  closeWizard: boolean;
}

/**
 * Decide how the create-instance wizard reacts to a failure.
 *
 * Only an instance that this attempt actually created counts as a partial
 * success. A duplicate name that already existed before the attempt is a real
 * creation error: claiming "created, but incomplete" there would be false, and
 * closing the wizard would hide that the user simply needs another name.
 *
 * @param stage Where the operation failed.
 * @param createdByThisAttempt Whether the instance exists now and did not
 *   exist before this attempt started. Always true for `game-files`/`loader`.
 */
export function planCreateFailure(
  stage: CreateInstanceStage,
  createdByThisAttempt: boolean,
): CreateInstanceFailurePlan {
  const created = stage !== 'create' || createdByThisAttempt;
  if (!created) {
    return { toastKey: 'create_instance.failed', closeWizard: false };
  }
  return { toastKey: 'create_instance.created_but_incomplete', closeWizard: true };
}
