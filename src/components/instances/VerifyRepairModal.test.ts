import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { beforeEach, describe, expect, it } from 'vitest';
import { useLanguageStore } from '../../stores/languageStore';
import { RepairResultView } from './RepairResultView';
import {
  initialRepairResultState,
  repairResultReducer,
  type RepairResultState,
  type RepairSummary,
} from './repairResult';

function summary(
  repaired: number,
  failed: number,
  errors: string[] = [],
): RepairSummary {
  return { repaired, failed, content_skipped: 0, errors };
}

function render(state: RepairResultState): string {
  useLanguageStore.setState({ language: 'en' });
  return renderToStaticMarkup(createElement(RepairResultView, state));
}

function finished(summaryValue: RepairSummary): RepairResultState {
  return repairResultReducer(initialRepairResultState, {
    type: 'repair_finished',
    summary: summaryValue,
  });
}

beforeEach(() => {
  useLanguageStore.setState({ language: 'en' });
});

describe('RepairResultView', () => {
  it('shows a successful Repair without a failure claim', () => {
    const html = render(finished(summary(3, 0)));
    expect(html).toContain('Repaired 3 file(s)');
    expect(html).not.toContain('Repair incomplete');
    expect(html).not.toContain('Repair failed');
  });

  it('shows a partially successful Repair with counters and error details', () => {
    const html = render(
      finished(summary(2, 1, ['loader: ambiguous artifact URL'])),
    );
    expect(html).toContain('Repair incomplete: 2 repaired, 1 failed');
    expect(html).toContain('Repair errors:');
    expect(html).toContain('loader: ambiguous artifact URL');
    expect(html).not.toContain('Repaired 2 file(s)');
  });

  it('shows a completely unsuccessful Repair as an error', () => {
    const state = repairResultReducer(initialRepairResultState, {
      type: 'repair_failed',
      error: 'repair command failed',
    });
    const html = render(state);
    expect(html).toContain('Repair failed: repair command failed');
    expect(html).not.toContain('Repaired 0 file(s)');
  });

  it('renders every error from a Repair with multiple failed operations', () => {
    const html = render(
      finished(
        summary(1, 3, [
          'first failure',
          'second failure',
          'third failure',
        ]),
      ),
    );
    expect(html).toContain('first failure');
    expect(html).toContain('second failure');
    expect(html).toContain('third failure');
    expect(html).toContain('1 repaired, 3 failed');
  });

  it('keeps the Repair result after the automatic Verify starts', () => {
    const repaired = finished(summary(2, 1, ['still broken']));
    const afterVerify = repairResultReducer(repaired, {
      type: 'verify_started',
    });
    expect(afterVerify).toBe(repaired);
    expect(afterVerify.summary?.failed).toBe(1);
    expect(render(afterVerify)).toContain('Repair incomplete: 2 repaired, 1 failed');
  });

  it('keeps a thrown Repair error after a subsequent Verify starts', () => {
    const failed = repairResultReducer(initialRepairResultState, {
      type: 'repair_failed',
      error: 'network down',
    });
    const afterVerify = repairResultReducer(failed, {
      type: 'verify_started',
    });
    expect(afterVerify.error).toBe('network down');
    expect(render(afterVerify)).toContain('Repair failed: network down');
  });

  it('clears the previous result when Repair runs again', () => {
    const previous = finished(summary(2, 1, ['old error']));
    const started = repairResultReducer(previous, { type: 'repair_started' });
    expect(started).toEqual(initialRepairResultState);
    expect(render(started)).not.toContain('old error');

    const next = repairResultReducer(started, {
      type: 'repair_finished',
      summary: summary(1, 0),
    });
    expect(render(next)).toContain('Repaired 1 file(s)');
  });

  it('resets on close and on reopening the modal', () => {
    const previous = finished(summary(1, 2, ['one', 'two']));
    const closed = repairResultReducer(previous, { type: 'reset' });
    expect(closed).toEqual(initialRepairResultState);
    expect(render(closed)).toBe('');
  });
});
