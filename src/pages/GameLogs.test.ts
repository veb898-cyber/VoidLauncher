// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { useLanguageStore } from '../stores/languageStore';
import { GameLogs } from './GameLogs';

const { invokeMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(async () => () => {}),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }));
vi.mock('@tauri-apps/api/event', () => ({ listen: listenMock }));
vi.mock('@tanstack/react-virtual', () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    containerRef: { current: null },
    getVirtualItems: () =>
      Array.from({ length: count }, (_, index) => ({
        index,
        key: index,
        start: index * 22,
        size: 22,
      })),
    measureElement: () => {},
    scrollToIndex: () => {},
  }),
}));

interface LogSession {
  path: string;
  instance_name: string;
  started_at: string;
  size_bytes: number;
}

interface VerifyCall {
  instanceName: string;
  scope: string;
  mode: string;
}

let container: HTMLDivElement;
let root: Root;
let verifyCalls: VerifyCall[];

function session(path: string, instanceName: string): LogSession {
  return {
    path,
    instance_name: instanceName,
    started_at: '23/11/2026 10:00:00',
    size_bytes: 128,
  };
}

function report(instanceName: string): Record<string, unknown> {
  const healthy = instanceName === 'Alpha';
  return {
    categories: [
      {
        id: 'assets',
        checked: 1,
        issue_count: healthy ? 0 : 1,
        issues: healthy
          ? []
          : [{ path: 'assets/indexes/5.json', kind: 'missing', repairable: true }],
        verified: healthy ? 1 : 0,
        unverified: 0,
      },
    ],
    repairable_count: healthy ? 0 : 1,
    content_issues: 0,
    healthy,
    mode: 'deep',
  };
}

function analysis(): Record<string, unknown> {
  return {
    verdict: 'likely',
    kind: 'mod_crash',
    culprit: 'example',
    detail: 'Controlled test crash',
    source: 'session-log',
    report_path: null,
    actions: [{ kind: 'repair_files', arg: '', tone: 'secondary' }],
  };
}

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
  Element.prototype.scrollTo = () => {};
});

beforeEach(() => {
  useLanguageStore.setState({ language: 'en' });
  verifyCalls = [];
  invokeMock.mockReset();
  listenMock.mockReset();
  listenMock.mockImplementation(async () => () => {});
  invokeMock.mockImplementation((command: string, args?: VerifyCall) => {
    switch (command) {
      case 'cmd_list_game_logs':
        return Promise.resolve([session('alpha.log', 'Alpha'), session('beta.log', 'Beta')]);
      case 'cmd_get_current_game_log':
        return Promise.resolve(null);
      case 'cmd_read_game_log_incremental':
        return Promise.resolve({
          lines: ['[10:00:00] [Client] controlled log line'],
          new_byte_offset: 1,
          reset: true,
          file_token: 'test-token',
        });
      case 'cmd_analyze_crash':
        return Promise.resolve(analysis());
      case 'cmd_verify_instance':
        verifyCalls.push(args!);
        return Promise.resolve(report(args!.instanceName));
      default:
        return Promise.reject(new Error(`unexpected command: ${command}`));
    }
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
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (predicate()) return;
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
  throw new Error(`timed out waiting for ${label}`);
}

async function renderPage(): Promise<void> {
  await act(async () => {
    root.render(createElement(GameLogs));
  });
  await flush();
  await waitFor(() => bodyText().includes('Alpha'), 'initial Alpha session');
}

function bodyText(): string {
  return document.body.textContent ?? '';
}

async function clickSelector(selector: string): Promise<void> {
  const element = document.querySelector<HTMLElement>(selector);
  if (!element) throw new Error(`element not found: ${selector}`);
  await act(async () => {
    element.click();
  });
  await flush();
}

async function clickButton(label: string): Promise<void> {
  const button = Array.from(document.querySelectorAll('button')).find(
    (candidate) => candidate.textContent?.trim() === label,
  );
  if (!button) throw new Error(`button not found: ${label}`);
  await act(async () => {
    button.click();
  });
  await flush();
}

async function clickAria(label: string): Promise<void> {
  await clickSelector(`button[aria-label="${label}"]`);
}

async function clickSession(instanceName: string): Promise<void> {
  const row = Array.from(document.querySelectorAll<HTMLElement>('.game-logs-picker-row')).find(
    (candidate) => candidate.textContent?.includes(instanceName),
  );
  if (!row) throw new Error(`session row not found: ${instanceName}`);
  await act(async () => {
    row.click();
  });
  await flush();
}

async function openVerifyForShownSession(): Promise<void> {
  await clickAria('Crash Analysis');
  await waitFor(() => bodyText().includes('Check files'), 'crash repair action');
  await clickButton('Check files');
  await waitFor(() => bodyText().includes('All files are in order') || bodyText().includes('Repairable issues: 1'), 'Verify report');
}

async function closeVerifyModal(): Promise<void> {
  const dialog = document.querySelector('[role="dialog"]');
  if (!dialog) throw new Error('Verify modal is not open');
  const close = Array.from(dialog.querySelectorAll('button')).find(
    (button) => button.textContent?.trim() === 'Close',
  );
  if (!close) throw new Error('Verify modal close button not found');
  await act(async () => {
    close.click();
  });
  await flush();
}

describe('GameLogs Verify/Repair page integration', () => {
  it('preserves the real page wiring while switching Alpha -> Beta and reopening', async () => {
    await renderPage();

    await openVerifyForShownSession();
    expect(verifyCalls).toEqual([{ instanceName: 'Alpha', scope: 'all', mode: 'deep' }]);
    expect(bodyText()).toContain('All files are in order');

    // Switch the shown session while the modal is open. The page-owned
    // key must remount the modal for Beta and issue a fresh Verify.
    await clickSelector('.game-logs-picker-trigger');
    await waitFor(() => bodyText().includes('Beta'), 'Beta session row');
    await clickSession('Beta');
    await waitFor(() => verifyCalls.length === 2, 'Beta Verify');
    expect(verifyCalls[1]).toEqual({ instanceName: 'Beta', scope: 'all', mode: 'deep' });
    expect(bodyText()).not.toContain('All files are in order');
    await waitFor(() => bodyText().includes('Repairable issues: 1'), 'Beta report');

    // Close and reopen: the same instance must still run a fresh Verify.
    await closeVerifyModal();
    await openVerifyForShownSession();
    expect(verifyCalls).toHaveLength(3);
    expect(verifyCalls[2]).toEqual({ instanceName: 'Beta', scope: 'all', mode: 'deep' });
  });
});
