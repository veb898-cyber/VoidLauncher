/**
 * Theme bridge for the sandboxed mod-list preview.
 *
 * The preview lives in an iframe, so it cannot inherit the launcher's CSS.
 * Instead of hardcoding a dark or white page, we read the live theme tokens
 * from the document root and inject them into the generated document. The
 * palette is re-read whenever the appearance attributes change, so switching
 * between a normal theme and Liquid Glass updates the preview in place.
 */

export interface PreviewPalette {
  background: string;
  surface: string;
  text: string;
  textSecondary: string;
  border: string;
  link: string;
  radius: string;
  fontUi: string;
  fontMono: string;
}

export const FALLBACK_PREVIEW_PALETTE: PreviewPalette = {
  background: '#16181d',
  surface: '#1f2229',
  text: '#f2f3f5',
  textSecondary: '#b9bcc6',
  border: 'rgba(255, 255, 255, 0.12)',
  link: '#9d8cff',
  radius: '10px',
  fontUi: '"Segoe UI", system-ui, sans-serif',
  fontMono: '"Cascadia Code", Consolas, monospace',
};

function readToken(
  styles: CSSStyleDeclaration,
  name: string,
  fallback: string,
): string {
  const value = styles.getPropertyValue(name).trim();
  return value || fallback;
}

/** Live values of the tokens the preview needs. */
export function readPreviewPalette(): PreviewPalette {
  if (typeof window === 'undefined' || typeof document === 'undefined') {
    return FALLBACK_PREVIEW_PALETTE;
  }
  const styles = getComputedStyle(document.documentElement);
  return {
    background: readToken(styles, '--bg-primary', FALLBACK_PREVIEW_PALETTE.background),
    surface: readToken(styles, '--bg-tertiary', FALLBACK_PREVIEW_PALETTE.surface),
    text: readToken(styles, '--text-primary', FALLBACK_PREVIEW_PALETTE.text),
    textSecondary: readToken(
      styles,
      '--text-secondary',
      FALLBACK_PREVIEW_PALETTE.textSecondary,
    ),
    border: readToken(styles, '--surface-border', FALLBACK_PREVIEW_PALETTE.border),
    link: readToken(styles, '--primary', FALLBACK_PREVIEW_PALETTE.link),
    radius: readToken(styles, '--radius-md', FALLBACK_PREVIEW_PALETTE.radius),
    fontUi: readToken(styles, '--font-ui', FALLBACK_PREVIEW_PALETTE.fontUi),
    fontMono: readToken(styles, '--font-mono', FALLBACK_PREVIEW_PALETTE.fontMono),
  };
}

/** Read the CSP nonce Tauri assigns to the bundled stylesheet. */
export function readStyleNonce(): string {
  if (typeof document === 'undefined') return '';
  for (const style of Array.from(document.querySelectorAll('style'))) {
    if (style.nonce) return style.nonce;
  }
  return '';
}

/** Never let a token value terminate the injected style element. */
function cssValue(value: string): string {
  return value.replace(/[<>{}\r\n]+/g, ' ').trim();
}

function styleTag(palette: PreviewPalette, nonce: string): string {
  const safeNonce = nonce.replace(/["'<>\s]+/g, '');
  const nonceAttr = safeNonce ? ` nonce="${safeNonce}"` : '';
  return `<style${nonceAttr}>${previewThemeCss(palette)}</style>`;
}

export function previewThemeCss(palette: PreviewPalette): string {
  const background = cssValue(palette.background);
  const surface = cssValue(palette.surface);
  const text = cssValue(palette.text);
  const secondary = cssValue(palette.textSecondary);
  const border = cssValue(palette.border);
  const link = cssValue(palette.link);
  const radius = cssValue(palette.radius);
  const fontUi = cssValue(palette.fontUi);
  const fontMono = cssValue(palette.fontMono);

  return `
:root { color-scheme: dark; }
html, body {
  margin: 0;
  padding: 10px 12px;
  background: ${background};
  color: ${text};
  font-family: ${fontUi};
  font-size: 13px;
  line-height: 1.55;
}
body { min-height: 100%; }
/* Wide HTML tables can make the rendered preview scroll both ways. Keep the
   scrollbar intersection on the preview background instead of the browser
   default white square. */
::-webkit-scrollbar-corner { background: ${background}; }
body > :first-child { margin-top: 0; }
a { color: ${link}; text-decoration: underline; text-underline-offset: 2px; }
h1, h2, h3, h4, h5, h6 {
  color: ${text};
  line-height: 1.25;
  margin: 0.65em 0 0.3em;
}
h1 { font-size: 1.35em; }
h2 { font-size: 1.2em; }
h3 { font-size: 1.08em; }
h4, h5, h6 { font-size: 1em; }
p { margin: 0.35em 0; }
ul, ol { margin: 0.35em 0; padding-left: 1.35em; }
li { margin: 0.1em 0; }
hr { border: 0; border-top: 1px solid ${border}; margin: 0.8em 0; }
blockquote {
  margin: 0.5em 0;
  padding: 0 0 0 10px;
  border-left: 3px solid ${border};
  color: ${secondary};
}
code, pre { font-family: ${fontMono}; font-size: 0.94em; }
code {
  padding: 0 3px;
  border: 1px solid ${border};
  border-radius: 4px;
  background: ${surface};
}
pre {
  padding: 8px;
  border: 1px solid ${border};
  border-radius: ${radius};
  background: ${surface};
  overflow: auto;
}
table { border-collapse: collapse; }
th, td { padding: 3px 6px; border: 1px solid ${border}; }
img { max-width: 100%; }
`;
}

/**
 * Inject the theme into a complete HTML document, or wrap an HTML fragment.
 * `style` is inserted right after `<head>` so custom template styles that
 * follow it can still override the defaults.
 */
export function injectPreviewTheme(
  content: string,
  palette: PreviewPalette,
  nonce = '',
): string {
  const style = styleTag(palette, nonce);
  const charset = /<meta[^>]+charset[^>]*>/i;
  if (charset.test(content)) {
    return content.replace(charset, (match) => `${match}${style}`);
  }
  const head = /<head[^>]*>/i;
  if (head.test(content)) {
    return content.replace(
      head,
      (match) => `${match}<meta charset="utf-8">${style}`,
    );
  }
  const html = /<html[^>]*>/i;
  if (html.test(content)) {
    return content.replace(
      html,
      (match) => `${match}<head><meta charset="utf-8">${style}</head>`,
    );
  }
  return `<!DOCTYPE html><html><head><meta charset="utf-8">${style}</head><body>${content}</body></html>`;
}

/** Wrap already-sanitized Markdown HTML in the same themed document. */
export function wrapPreviewBody(
  body: string,
  palette: PreviewPalette,
  nonce = '',
): string {
  return `<!DOCTYPE html><html><head><meta charset="utf-8">${styleTag(palette, nonce)}</head><body>${body}</body></html>`;
}
