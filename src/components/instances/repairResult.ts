/**
 * State for the Repair result that must survive the automatic Verify that
 * follows a Repair. Keeping it separate from the Verify report prevents a
 * fresh report from erasing a failed/partial Repair summary or its errors.
 */
export interface RepairSummary {
  repaired: number;
  failed: number;
  content_skipped: number;
  errors: string[];
}

export interface RepairResultState {
  summary: RepairSummary | null;
  error: string | null;
}

export type RepairResultAction =
  | { type: 'reset' }
  | { type: 'verify_started' }
  | { type: 'repair_started' }
  | { type: 'repair_finished'; summary: RepairSummary }
  | { type: 'repair_failed'; error: string };

export const initialRepairResultState: RepairResultState = {
  summary: null,
  error: null,
};

/**
 * A Verify start deliberately does not appear here: Verify changes its own
 * report and error state, but it must never clear the last Repair result.
 */
export function repairResultReducer(
  _state: RepairResultState,
  action: RepairResultAction,
): RepairResultState {
  switch (action.type) {
    case 'reset':
    case 'repair_started':
      return { summary: null, error: null };
    case 'verify_started':
      // Deliberately a no-op: the automatic Verify after Repair must not erase
      // the operation's counters or error details.
      return _state;
    case 'repair_finished':
      return { summary: action.summary, error: null };
    case 'repair_failed':
      return { summary: null, error: action.error };
  }
}
