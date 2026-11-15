import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { ModListPreview } from '../components/instances/ModListPreview';
import { FALLBACK_PREVIEW_PALETTE } from './modListPreviewTheme';

describe('mod list preview is manually selectable', () => {
  it('renders the generated text inside a read-only textarea', () => {
    const content = 'Alpha — 1.0.0\nBeta & Co';
    const markup = renderToStaticMarkup(
      createElement(ModListPreview, { content, kind: 'text' }),
    );
    expect(markup).toContain('<textarea');
    expect(markup.toLowerCase()).toContain('readonly');
    expect(markup).toContain('Alpha — 1.0.0');
    expect(markup).toContain('Beta &amp; Co');
    expect(markup).not.toContain('<iframe');
  });

  it('renders HTML/Markdown results only inside a script-less sandboxed iframe', () => {
    const content = '<!DOCTYPE html><html><body><p>Preview</p></body></html>';
    const markup = renderToStaticMarkup(
      createElement(ModListPreview, { content, kind: 'html' }),
    );
    expect(markup).toContain('<iframe');
    expect(markup).toContain('sandbox=""');
    expect(markup).toContain('Preview');
    expect(markup).toContain('<textarea');
    expect(markup).toContain(FALLBACK_PREVIEW_PALETTE.background);
    expect(markup).not.toContain('background: #ffffff');
  });
});
