import { describe, expect, it } from 'vitest';
import {
  FALLBACK_PREVIEW_PALETTE,
  injectPreviewTheme,
  previewThemeCss,
  readPreviewPalette,
  wrapPreviewBody,
  type PreviewPalette,
} from './modListPreviewTheme';

const DARK: PreviewPalette = {
  background: 'rgb(10, 11, 14)',
  surface: 'rgb(24, 26, 32)',
  text: 'rgb(240, 241, 244)',
  textSecondary: 'rgb(170, 175, 190)',
  border: 'rgba(255, 255, 255, 0.12)',
  link: 'rgb(120, 160, 255)',
  radius: '10px',
  fontUi: '"Segoe UI", sans-serif',
  fontMono: '"Cascadia Code", monospace',
};

const LIGHT: PreviewPalette = {
  ...DARK,
  background: 'rgb(240, 240, 244)',
  surface: 'rgb(225, 226, 232)',
  text: 'rgb(20, 21, 24)',
  link: 'rgb(40, 90, 200)',
};

describe('preview theme palette', () => {
  it('uses the fallback when no DOM is available', () => {
    expect(readPreviewPalette()).toEqual(FALLBACK_PREVIEW_PALETTE);
  });

  it('styles background, text, links, headings, lists, code and separators', () => {
    const css = previewThemeCss(DARK);
    expect(css).toContain(DARK.background);
    expect(css).toContain(DARK.text);
    expect(css).toContain(DARK.link);
    expect(css).toContain(DARK.border);
    expect(css).toContain(DARK.surface);
    expect(css).toContain(DARK.fontUi);
    expect(css).toContain(DARK.fontMono);
    expect(css).toContain('h1, h2, h3, h4, h5, h6');
    expect(css).toContain('ul, ol');
    expect(css).toContain('pre');
    expect(css).toContain('hr');
    expect(css).toContain(`::-webkit-scrollbar-corner { background: ${DARK.background}; }`);
    expect(css).not.toContain('#ffffff');
  });

  it('changes the document when the theme changes', () => {
    const dark = previewThemeCss(DARK);
    const light = previewThemeCss(LIGHT);
    expect(dark).not.toBe(light);
    expect(light).toContain(LIGHT.background);
    expect(light).toContain(LIGHT.link);
  });
});

describe('themed preview documents', () => {
  it('injects the theme into an existing full HTML document', () => {
    const html =
      '<!DOCTYPE html><html><head><meta charset="utf-8"><title>List</title></head><body><ul><li>Alpha</li></ul></body></html>';
    const themed = injectPreviewTheme(html, DARK);
    expect(themed).toContain('<meta charset="utf-8"><style>');
    expect(themed).toContain(DARK.background);
    expect(themed).toContain('<title>List</title>');
    expect(themed).toContain('<li>Alpha</li>');
  });

  it('carries the CSP nonce so the theme is allowed inside the iframe', () => {
    const themed = injectPreviewTheme('<p>x</p>', DARK, 'nonce-123');
    expect(themed).toContain('<style nonce="nonce-123">');
    const markdown = wrapPreviewBody('<p>x</p>', DARK, 'nonce-123');
    expect(markdown).toContain('<style nonce="nonce-123">');
  });

  it('wraps an HTML fragment without losing its content', () => {
    const themed = injectPreviewTheme('<li>Alpha</li>', DARK);
    expect(themed.startsWith('<!DOCTYPE html>')).toBe(true);
    expect(themed).toContain('<body><li>Alpha</li></body>');
    expect(themed).toContain(DARK.background);
  });

  it('wraps sanitized Markdown with the same theme', () => {
    const themed = wrapPreviewBody('<ul><li>Alpha</li></ul>', LIGHT);
    expect(themed).toContain('<ul><li>Alpha</li></ul>');
    expect(themed).toContain(LIGHT.background);
    expect(themed).toContain(LIGHT.text);
  });

  it('does not let a token close the style element', () => {
    const themed = injectPreviewTheme('<p>x</p>', {
      ...DARK,
      background: 'red</style><script>alert(1)</script>',
    });
    expect(themed).not.toContain('<script>alert(1)</script>');
  });
});
