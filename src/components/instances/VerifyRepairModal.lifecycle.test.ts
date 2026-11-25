// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { useLanguageStore } from '../../stores/languageStore';
import { VerifyRepairModal } from './VerifyRepairModal';

const { invokeMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(async () => () => {}),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }));
vi.mock('@tauri-apps/api/event', () => ({ listen: listenMock }));

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function report(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    categories: [
      {
        id: 'assets',
        checked: 1,
        issue_count: 0,
        issues: [],
        verified: 1,
        unverified: 0,
      },
    ],
    repairable_count: 0,
    content_issues: 0,
    healthy: true,
    mode: 'deep',
    ...overrides,
  };
}

function repairableReport(): Record<string, unknown> {
  return report({
    healthy: false,
    repairable_count: 1,
    categories: [
      {
        id: 'assets',
        checked: 1,
        issue_count: 1,
        issues: [{ path: 'assets/indexes/5.json', kind: 'missing', repairable: true }],
        verified: 0,
        unverified: 0,
      },
    ],
  });
}

function issueReport(
  issues: Array<Record<string, unknown>>,
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return report({
    healthy: false,
    repairable_count: issues.filter((issue) => issue.repairable === true).length,
    categories: [
      {
        id: 'loader',
        checked: 1,
        issue_count: issues.length,
        issues,
        verified: 0,
        unverified: 0,
      },
    ],
    ...overrides,
  });
}

let container: HTMLDivElement;
let root: Root;
let verifyQueue: Deferred<Record<string, unknown>>[];
let repairQueue: Deferred<Record<string, unknown>>[];

beforeAll(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) =>
    window.setTimeout(() => callback(Date.now()), 0),
  );
  vi.stubGlobal('cancelAnimationFrame', (handle: number) => window.clearTimeout(handle));
  vi.stubGlobal('matchMedia', (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  }));
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
});

beforeEach(() => {
  useLanguageStore.setState({ language: 'en' });
  verifyQueue = [];
  repairQueue = [];
  invokeMock.mockReset();
  listenMock.mockReset();
  listenMock.mockImplementation(async () => () => {});
  invokeMock.mockImplementation((command: string) => {
    if (command === 'cmd_verify_instance') {
      const pending = deferred<Record<string, unknown>>();
      verifyQueue.push(pending);
      return pending.promise;
    }
    if (command === 'cmd_repair_instance') {
      const pending = deferred<Record<string, unknown>>();
      repairQueue.push(pending);
      return pending.promise;
    }
    return Promise.reject(new Error(`unexpected command: ${command}`));
  });
  container = document.createElement('div');
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => {
    root.unmount();
  });
  container.remove();
});

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function renderModal(open: boolean, instanceName = 'Alpha'): Promise<void> {
  await act(async () => {
    root.render(
      createElement(VerifyRepairModal, {
        open,
        instanceName,
        onClose: () => {},
      }),
    );
  });
  await flush();
}

async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
  throw new Error(`timed out waiting for ${label}`);
}

function bodyText(): string {
  return document.body.textContent ?? '';
}

async function pendingVerify(): Promise<Deferred<Record<string, unknown>>> {
  await waitFor(() => verifyQueue.length > 0, 'Verify request');
  return verifyQueue.shift()!;
}

async function pendingRepair(): Promise<Deferred<Record<string, unknown>>> {
  await waitFor(() => repairQueue.length > 0, 'Repair request');
  return repairQueue.shift()!;
}

async function clickButton(label: string): Promise<void> {
  const button = Array.from(document.querySelectorAll('button')).find((candidate) =>
    candidate.textContent?.includes(label),
  );
  if (!button) throw new Error(`button not found: ${label}`);
  await act(async () => {
    button.click();
  });
  await flush();
}

describe('VerifyRepairModal lifecycle', () => {
  it('runs Verify when the modal opens', async () => {
    await renderModal(true);
    expect(invokeMock).toHaveBeenCalledWith('cmd_verify_instance', {
      instanceName: 'Alpha',
      scope: 'all',
      mode: 'deep',
    });
    const pending = await pendingVerify();
    pending.resolve(report());
    await flush();
    await waitFor(() => bodyText().includes('All files are in order'), 'healthy report');
  });

  it('does not reuse a report after close and reopen', async () => {
    await renderModal(true);
    const first = await pendingVerify();
    first.resolve(report());
    await flush();
    expect(bodyText()).toContain('All files are in order');

    await renderModal(false);
    expect(bodyText()).not.toContain('All files are in order');

    await renderModal(true);
    expect(invokeMock).toHaveBeenCalledTimes(2);
    const second = await pendingVerify();
    second.resolve(repairableReport());
    await flush();
    expect(bodyText()).toContain('Repairable issues: 1');
  });

  it('never shows the previous instance report after an instance switch', async () => {
    await renderModal(true, 'Alpha');
    const alpha = await pendingVerify();
    alpha.resolve(report());
    await flush();
    expect(bodyText()).toContain('All files are in order');

    await renderModal(true, 'Beta');
    expect(invokeMock).toHaveBeenLastCalledWith('cmd_verify_instance', {
      instanceName: 'Beta',
      scope: 'all',
      mode: 'deep',
    });
    expect(bodyText()).not.toContain('All files are in order');

    const beta = await pendingVerify();
    beta.resolve(repairableReport());
    await flush();
    expect(bodyText()).toContain('Repairable issues: 1');
  });

  it('ignores a late response from the previous instance', async () => {
    await renderModal(true, 'Alpha');
    const alpha = await pendingVerify();

    await renderModal(true, 'Beta');
    const beta = await pendingVerify();
    beta.resolve(repairableReport());
    await flush();
    expect(bodyText()).toContain('Repairable issues: 1');

    alpha.resolve(report());
    await flush();
    expect(bodyText()).toContain('Repairable issues: 1');
    expect(bodyText()).not.toContain('All files are in order');

    await renderModal(true, 'Alpha');
    expect(invokeMock).toHaveBeenCalledTimes(3);
    const freshAlpha = await pendingVerify();
    freshAlpha.resolve(report());
    await flush();
    expect(bodyText()).toContain('All files are in order');
  });

  it('ignores a late response after the modal is closed', async () => {
    await renderModal(true);
    const first = await pendingVerify();
    await renderModal(false);
    first.resolve(report());
    await flush();
    expect(bodyText()).not.toContain('All files are in order');

    await renderModal(true);
    expect(invokeMock).toHaveBeenCalledTimes(2);
    const second = await pendingVerify();
    second.resolve(repairableReport());
    await flush();
    expect(bodyText()).toContain('Repairable issues: 1');
  });

  it('clears the old report when Repair fails', async () => {
    await renderModal(true);
    const verify = await pendingVerify();
    verify.resolve(repairableReport());
    await flush();
    expect(bodyText()).toContain('Repairable issues: 1');

    await clickButton('Repair');
    const repair = await pendingRepair();
    repair.reject(new Error('network down'));
    await flush();
    await waitFor(
      () => bodyText().includes('Repair failed') && bodyText().includes('network down'),
      'repair error',
    );
    expect(bodyText()).not.toContain('Repairable issues: 1');
    expect(bodyText()).not.toContain('All files are in order');
  });

  it('keeps a Repair error visible after a subsequent Verify', async () => {
    await renderModal(true);
    const verify = await pendingVerify();
    verify.resolve(repairableReport());
    await flush();
    await clickButton('Repair');
    const repair = await pendingRepair();
    repair.reject(new Error('network down'));
    await flush();
    await waitFor(
      () => bodyText().includes('Repair failed') && bodyText().includes('network down'),
      'repair error',
    );

    await clickButton('Re-verify');
    const reverify = await pendingVerify();
    reverify.resolve(report());
    await flush();
    expect(bodyText()).toContain('Repair failed');
    expect(bodyText()).toContain('network down');
    expect(bodyText()).toContain('All files are in order');
  });

  it('shows failed operations from a partial Repair', async () => {
    await renderModal(true);
    const verify = await pendingVerify();
    verify.resolve(repairableReport());
    await flush();

    await clickButton('Repair');
    const repair = await pendingRepair();
    repair.resolve({ repaired: 1, failed: 2, content_skipped: 0, errors: ['first', 'second'] });
    await flush();
    const reverify = await pendingVerify();
    reverify.resolve(report());
    await flush();

    expect(bodyText()).toContain('Repair incomplete: 1 repaired, 2 failed');
    expect(bodyText()).toContain('first');
    expect(bodyText()).toContain('second');
    expect(bodyText()).toContain('All files are in order');
  });

  it('updates the result on a repeated Repair', async () => {
    await renderModal(true);
    const verify = await pendingVerify();
    verify.resolve(repairableReport());
    await flush();

    await clickButton('Repair');
    const partial = await pendingRepair();
    partial.resolve({ repaired: 1, failed: 2, content_skipped: 0, errors: ['old error'] });
    await flush();
    const reverify = await pendingVerify();
    reverify.resolve(repairableReport());
    await flush();
    expect(bodyText()).toContain('Repair incomplete: 1 repaired, 2 failed');
    expect(bodyText()).toContain('old error');

    await clickButton('Repair');
    const success = await pendingRepair();
    success.resolve({ repaired: 3, failed: 0, content_skipped: 0, errors: [] });
    await flush();
    const finalVerify = await pendingVerify();
    finalVerify.resolve(report());
    await flush();
    expect(bodyText()).toContain('Repaired 3 file(s)');
    expect(bodyText()).not.toContain('old error');
  });

  it('does not resurrect a cached Healthy report after a failed forced re-verify', async () => {
    await renderModal(true);
    const initial = await pendingVerify();
    initial.resolve(report());
    await flush();
    expect(bodyText()).toContain('All files are in order');

    await clickButton('Re-verify');
    const forced = await pendingVerify();
    forced.reject(new Error('forced scan failed'));
    await flush();
    await waitFor(
      () => bodyText().includes('Verification failed: Error: forced scan failed'),
      'forced verify error',
    );

    // Move away and back to the same scope. The failed forced scan must have
    // invalidated the old Healthy cache entry; returning must issue a fresh
    // request instead of showing the pre-failure report.
    await clickButton('Launcher files');
    const launcher = await pendingVerify();
    launcher.resolve(repairableReport());
    await flush();

    await clickButton('All');
    const freshAll = await pendingVerify();
    expect(bodyText()).not.toContain('All files are in order');
    freshAll.resolve(repairableReport());
    await flush();
    expect(bodyText()).toContain('Repairable issues: 1');
  });

  it('keeps cache entries isolated by instance', async () => {
    await renderModal(true, 'Alpha');
    const alpha = await pendingVerify();
    alpha.resolve(report());
    await flush();

    await renderModal(true, 'Beta');
    const beta = await pendingVerify();
    beta.resolve(repairableReport());
    await flush();
    expect(bodyText()).toContain('Repairable issues: 1');

    await renderModal(true, 'Alpha');
    expect(invokeMock).toHaveBeenCalledTimes(3);
    const alphaAgain = await pendingVerify();
    alphaAgain.resolve(report());
    await flush();
    expect(bodyText()).toContain('All files are in order');
  });

  it('renders localized generation labels and neutral multi-line detail', async () => {
    await renderModal(true);
    const pending = await pendingVerify();
    const longDetail =
      'the generation manifest is missing; safe regeneration is required\n'
      + 'path: libraries/example.jar';
    pending.resolve(
      issueReport([
        {
          path: 'loader-generation',
          kind: 'missing_generation_manifest',
          repairable: true,
          detail: longDetail,
        },
        {
          path: 'libraries/example.jar',
          kind: 'corrupt_generated_output',
          repairable: true,
          detail: 'output failed its publisher checksum',
        },
      ]),
    );
    await flush();
    await waitFor(
      () => bodyText().includes('generation manifest is missing'),
      'English generation issue labels',
    );
    expect(bodyText()).toContain('generated output is corrupt');
    expect(bodyText()).toContain(longDetail);
    expect(bodyText()).not.toContain('requires ');

    const details = Array.from(
      document.querySelectorAll<HTMLElement>('[data-testid="verify-issue-detail"]'),
    );
    expect(details).toHaveLength(2);
    expect(details[0].style.whiteSpace).toBe('pre-wrap');
    expect(details[0].style.overflowWrap).toBe('anywhere');

    await act(async () => {
      useLanguageStore.setState({ language: 'ru' });
    });
    await flush();
    expect(bodyText()).toContain('нет манифеста generation');
    expect(bodyText()).toContain('сгенерированный файл повреждён');
  });

  it('renders an unknown generation kind with a readable escaped fallback', async () => {
    await renderModal(true);
    const pending = await pendingVerify();
    pending.resolve(
      issueReport([
        {
          path: 'loader-generation',
          kind: 'future_generation_kind',
          repairable: false,
          detail: '<script>alert(1)</script>\nline two',
        },
      ]),
    );
    await flush();
    await waitFor(
      () => bodyText().includes('unknown issue (future_generation_kind)'),
      'unknown issue fallback',
    );
    expect(bodyText()).toContain('<script>alert(1)</script>');
    expect(document.querySelector('script')).toBeNull();
    const detail = document.querySelector<HTMLElement>(
      '[data-testid="verify-issue-detail"]',
    );
    expect(detail?.textContent).toContain('line two');
  });

  it('does not render an empty detail block', async () => {
    await renderModal(true);
    const pending = await pendingVerify();
    pending.resolve(
      issueReport([
        { path: 'loader-generation', kind: 'missing', repairable: true, detail: '' },
        { path: 'libraries/example.jar', kind: 'corrupt', repairable: true },
      ]),
    );
    await flush();
    await waitFor(() => bodyText().includes('missing'), 'issues without detail');
    expect(document.querySelectorAll('[data-testid="verify-issue-detail"]')).toHaveLength(0);
  });

  it('keeps legacy issue labels and shows loader range as neutral detail', async () => {
    await renderModal(true);
    const pending = await pendingVerify();
    pending.resolve(
      issueReport([
        { path: 'mods/example.jar', kind: 'outdated_loader', repairable: false, detail: '>=0.19.5' },
        { path: 'assets/indexes/5.json', kind: 'unreadable', repairable: false },
      ]),
    );
    await flush();
    await waitFor(
      () => bodyText().includes('needs a newer Fabric Loader'),
      'legacy issue labels',
    );
    expect(bodyText()).toContain('>=0.19.5');
    expect(bodyText()).toContain('unreadable');
    expect(bodyText()).not.toContain('requires ');
  });
});
