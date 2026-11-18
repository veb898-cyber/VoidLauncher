import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { beforeEach, describe, expect, it } from 'vitest';

function installMinimalDom(): void {
  const g = globalThis as unknown as { document?: unknown; window?: unknown };
  if (!g.document) {
    g.document = {
      documentElement: {
        lang: 'en',
        style: {},
        setAttribute: () => {},
        removeAttribute: () => {},
      },
    };
  }
  if (!g.window) {
    g.window = {
      matchMedia: () => ({
        matches: false,
        addEventListener: () => {},
        removeEventListener: () => {},
      }),
    };
  }
}

async function renderSlidingTabs(props: any): Promise<string> {
  installMinimalDom();
  const { SlidingTabs } = await import('./SlidingTabs');
  return renderToStaticMarkup(createElement(SlidingTabs, props));
}

describe('SlidingTabs semantics', () => {
  beforeEach(() => installMinimalDom());

  it('links each tab to an existing tabpanel id', async () => {
    const html = await renderSlidingTabs({
      tabs: [
        { id: 'alpha', label: 'Alpha' },
        { id: 'beta', label: 'Beta' },
      ],
      active: 'beta',
      onChange: () => {},
      ariaLabel: 'Demo tabs',
      controlsPanels: true,
      idPrefix: 'demo',
    });

    expect(html).toContain('role="tablist"');
    expect(html).toContain('id="demo-tab-alpha"');
    expect(html).toContain('aria-controls="demo-panel-alpha"');
    expect(html).toContain('id="demo-tab-beta"');
    expect(html).toContain('aria-controls="demo-panel-beta"');
    expect(html).toContain('aria-selected="true"');
    expect(html).toContain('aria-selected="false"');
  });

  it('moves the single tab stop off a disabled active tab', async () => {
    const html = await renderSlidingTabs({
      tabs: [
        { id: 'alpha', label: 'Alpha' },
        { id: 'beta', label: 'Beta', disabled: true },
      ],
      active: 'beta',
      onChange: () => {},
      ariaLabel: 'Demo tabs',
      controlsPanels: true,
      idPrefix: 'disabled-demo',
    });

    expect(html).toMatch(/id="disabled-demo-tab-alpha"[^>]*tabindex="0"/);
    expect(html).toMatch(/id="disabled-demo-tab-beta"[^>]*tabindex="-1"/);
  });

  it('uses button-group semantics for filter rows without panels', async () => {
    const html = await renderSlidingTabs({
      tabs: [
        { id: 'all', label: 'All' },
        { id: 'mods', label: 'Mods' },
      ],
      active: 'mods',
      onChange: () => {},
      ariaLabel: 'Filter',
    });

    expect(html).toContain('role="group"');
    expect(html).not.toContain('role="tablist"');
    expect(html).toContain('aria-pressed="true"');
    expect(html).not.toContain('aria-controls=');
  });
});
