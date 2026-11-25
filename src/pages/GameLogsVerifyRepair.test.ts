// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { useLanguageStore } from '../stores/languageStore';
import { GameLogsVerifyRepair } from './GameLogsVerifyRepair';

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

let container: HTMLDivElement;
let root: Root;
let verifyQueue: Deferred<Record<string, unknown>>[];

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
  invokeMock.mockReset();
  listenMock.mockReset();
  listenMock.mockImplementation(async () => () => {});
  invokeMock.mockImplementation((command: string) => {
    if (command === 'cmd_verify_instance') {
      const pending = deferred<Record<string, unknown>>();
      verifyQueue.push(pending);
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

async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
  throw new Error(`timed out waiting for ${label}`);
}

async function render(
  session: { instance_name: string } | null,
  open: boolean,
): Promise<void> {
  await act(async () => {
    root.render(
      createElement(GameLogsVerifyRepair, {
        session,
        open,
        onClose: () => {},
      }),
    );
  });
  await flush();
}

async function pendingVerify(): Promise<Deferred<Record<string, unknown>>> {
  await waitFor(() => verifyQueue.length > 0, 'Verify request');
  return verifyQueue.shift()!;
}

function bodyText(): string {
  return document.body.textContent ?? '';
}

describe('GameLogs Verify/Repair integration', () => {
  it('switches instances and never shows the previous instance report', async () => {
    await render({ instance_name: 'Alpha' }, true);
    const alpha = await pendingVerify();
    alpha.resolve(report());
    await flush();
    expect(bodyText()).toContain('All files are in order');

    await render({ instance_name: 'Beta' }, true);
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

  it('runs a fresh Verify after close and reopen', async () => {
    await render({ instance_name: 'Alpha' }, true);
    const first = await pendingVerify();
    first.resolve(report());
    await flush();
    expect(bodyText()).toContain('All files are in order');

    await render({ instance_name: 'Alpha' }, false);
    expect(bodyText()).not.toContain('All files are in order');

    await render({ instance_name: 'Alpha' }, true);
    expect(invokeMock).toHaveBeenCalledTimes(2);
    const second = await pendingVerify();
    second.resolve(repairableReport());
    await flush();
    expect(bodyText()).toContain('Repairable issues: 1');
  });
});
