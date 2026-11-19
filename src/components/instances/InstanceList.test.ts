import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { beforeEach, describe, expect, it } from 'vitest';
import { useInstanceStore } from '../../stores/instanceStore';
import { useLanguageStore } from '../../stores/languageStore';

// The component graph pulls in visual stores that touch <html> at module load.
// The production code only does that in a real browser; this test renders the
// static markup, so provide the two properties those modules read.
function installMinimalDom(): void {
  const g = globalThis as unknown as {
    document?: unknown;
    window?: unknown;
  };
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

async function renderList(): Promise<string> {
  installMinimalDom();
  const { InstanceList } = await import('./InstanceList');
  return renderToStaticMarkup(createElement(InstanceList, { onCreateClick: () => {} }));
}

describe('InstanceList create action', () => {
  beforeEach(() => {
    useInstanceStore.setState({
      instances: [],
      selectedInstance: null,
      isLoading: false,
      error: null,
      isLaunching: false,
      launchStatus: null,
      elybySkinsPrompt: null,
    });
  });

  it('has a Russian accessible name for the icon-only create button', async () => {
    useLanguageStore.setState({ language: 'ru' });
    const html = await renderList();
    expect(html).toMatch(/<button[^>]*aria-label="Создать сборку"/);
  });

  it('has an English accessible name for the icon-only create button', async () => {
    useLanguageStore.setState({ language: 'en' });
    const html = await renderList();
    expect(html).toMatch(/<button[^>]*aria-label="Create Instance"/);
  });
});
