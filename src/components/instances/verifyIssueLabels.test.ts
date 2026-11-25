import { describe, expect, it } from 'vitest';
import { en } from '../../lib/i18n/en';
import { ru } from '../../lib/i18n/ru';
import {
  KIND_LABELS,
  KNOWN_ISSUE_KINDS,
  issueKindLabelKey,
} from './verifyIssueLabels';

describe('verify issue labels', () => {
  it('maps every known backend issue kind to English and Russian labels', () => {
    for (const kind of KNOWN_ISSUE_KINDS) {
      const key = KIND_LABELS[kind];
      expect(key, kind).toBeTruthy();
      expect(en[key], `${kind} (en)`).toBeTruthy();
      expect(ru[key], `${kind} (ru)`).toBeTruthy();
    }
  });

  it('has a readable fallback for an unknown future kind', () => {
    expect(issueKindLabelKey('future_generation_kind')).toBeUndefined();
    expect(en['verify.kind_unknown']).toContain('{kind}');
    expect(ru['verify.kind_unknown']).toContain('{kind}');
  });

  it('does not keep the unused repair-summary and Requires keys', () => {
    expect(en).not.toHaveProperty('verify.repaired');
    expect(ru).not.toHaveProperty('verify.repaired');
    expect(en).not.toHaveProperty('verify.issue_requires');
    expect(ru).not.toHaveProperty('verify.issue_requires');
  });
});
