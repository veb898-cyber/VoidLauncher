import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { useLanguageStore } from '../../stores/languageStore';

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

const item = {
  filename: 'example.jar',
  name: 'Example Mod',
  version: '1.0.0',
  version_known: true,
  provider: 'Modrinth',
  loaders: ['Fabric'],
  enabled: true,
  icon: null,
};

const handlers = {
  iconsResolving: false,
  compatTarget: '',
  onRowClick: () => {},
  onContext: () => {},
  onToggle: () => {},
};

async function renderRow(kind: 'table' | 'card'): Promise<string> {
  installMinimalDom();
  const { ContentRow, ContentCardRow } = await import('./ContentManager');
  useLanguageStore.setState({ language: 'en' });
  if (kind === 'table') {
    return renderToStaticMarkup(
      createElement(ContentRow, { item, idx: 0, isSelected: false, ...handlers }),
    );
  }
  return renderToStaticMarkup(
    createElement(ContentCardRow, { item, isSelected: false, ...handlers }),
  );
}

describe('content list row interaction', () => {
  it('keeps the enable checkbox separate from the row selection button', async () => {
    const html = await renderRow('table');
    expect(html).toContain('role="checkbox"');
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain('content-list-row-select');
    expect(html).toContain('aria-pressed="false"');
    expect(html).not.toMatch(/content-list-row[^>]*role="button"/);
  });

  it('uses the same split interaction in the card view', async () => {
    const html = await renderRow('card');
    expect(html).toContain('role="checkbox"');
    expect(html).toContain('content-list-card-select');
    expect(html).toContain('aria-pressed="false"');
    expect(html).not.toMatch(/content-list-card[^>]*role="button"/);
  });
});
