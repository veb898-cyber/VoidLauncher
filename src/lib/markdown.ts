import { marked } from 'marked';
import DOMPurify from 'dompurify';
import { invoke } from '@tauri-apps/api/core';

// Shared description renderer for Modrinth / CurseForge project pages.
//
// Modrinth bodies are CommonMark with GFM extensions (tables, strikethrough,
// autolink literals); CurseForge bodies arrive as HTML and are normalized to
// markdown before parsing. `marked` covers everything the platforms emit:
// bare URLs become links, [![img](src)](href) badges stay clickable, empty-alt
// images render. Output is sanitized with a strict allowlist.

export type MdSource = string | undefined;

function resolveUrl(url: string, source?: MdSource): string {
  if (!url) return url;
  const trimmed = url.trim();
  if (trimmed.startsWith('//')) return `https:${trimmed}`;
  // Relative paths in CF/Modrinth HTML resolve against the platform origin.
  if (trimmed.startsWith('/')) {
    const base = source === 'curseforge' ? 'https://www.curseforge.com' : 'https://modrinth.com';
    return `${base}${trimmed}`;
  }
  return trimmed;
}

export function renderMarkdownToHtml(text: string, source?: MdSource): string {
  if (!text) return '';
  // Normalize raw <img>/<a> HTML (CurseForge bodies) to markdown so the
  // marked pipeline gets a single consistent input.
  const clean = text
    .replace(/<img\s[^>]*>/gi, (m) => {
      const src = m.match(/src=["']([^"']+)["']/i);
      const altMatch = m.match(/alt=["']([^"']*)["']/i);
      const alt = altMatch ? altMatch[1] : '';
      return src ? `![${alt}](${resolveUrl(src[1], source)})` : '';
    })
    .replace(/<a[^>]+href=["']([^"']+)["'][^>]*>(.*?)<\/a>/gi, '[$2]($1)');
  try {
    const html = marked.parse(clean, { gfm: true, breaks: false }) as string;
    return DOMPurify.sanitize(html, {
      ALLOWED_TAGS: [
        'a', 'b', 'blockquote', 'br', 'code', 'del', 'div', 'em', 'h1',
        'h2', 'h3', 'h4', 'h5', 'h6', 'hr', 'i', 'img', 'ins', 'li',
        'ol', 'p', 'pre', 's', 'span', 'strong', 'sub', 'sup', 'table',
        'tbody', 'td', 'th', 'thead', 'tr', 'ul',
      ],
      ALLOWED_ATTR: ['href', 'src', 'alt', 'title'],
      ALLOWED_URI_REGEXP: /^(?:https?:|data:image\/)/i,
      FORBID_TAGS: ['script', 'iframe', 'object', 'embed', 'style', 'form', 'input', 'button'],
      FORBID_ATTR: ['onerror', 'onload', 'onclick', 'onmouseover', 'onfocus', 'onblur', 'onchange', 'onsubmit', 'style'],
    });
  } catch {
    return '';
  }
}

// ---- Remote image hydration -------------------------------------------------
// The webview loads <img src="https://..."> through the system proxy only —
// no proxy->direct fallback — so images on small hosts often fail to appear.
// After mounting, every remote image is re-fetched through the backend
// (cmd_fetch_page_asset: retries, direct fallback, SSRF guard) and swapped
// to a data URL. If the backend fetch fails too, the original src is kept as
// a last-resort fallback (same behavior as Prism Launcher).

const assetCache = new Map<string, string>(); // url -> data URL ('' = failed)
const pendingSubs = new Map<string, HTMLImageElement[]>();
let activeFetches = 0;
const fetchQueue: string[] = [];
const MAX_CONCURRENT = 6;

function startNext() {
  while (activeFetches < MAX_CONCURRENT && fetchQueue.length > 0) {
    const url = fetchQueue.shift()!;
    activeFetches++;
    invoke<string | null>('cmd_fetch_page_asset', { url })
      .then((data) => {
        if (data) {
          assetCache.set(url, data);
          for (const img of pendingSubs.get(url) ?? []) img.src = data;
        } else {
          assetCache.set(url, '');
        }
      })
      .catch(() => { assetCache.set(url, ''); })
      .finally(() => {
        pendingSubs.delete(url);
        activeFetches--;
        startNext();
      });
  }
}

/**
 * Swap every remote image inside `root` for a backend-fetched data URL.
 * Call after rendering markdown into the DOM (e.g. in an effect keyed on
 * the detail object).
 */
export function hydrateRemoteImages(root: HTMLElement | null | undefined): void {
  if (!root) return;
  const imgs = root.querySelectorAll('img');
  imgs.forEach((img) => {
    const src = img.getAttribute('src') ?? '';
    if (!/^https?:\/\//i.test(src)) return;
    const cached = assetCache.get(src);
    if (cached) { img.src = cached; return; }
    if (cached === '') return; // failed earlier this session; keep original src
    const subs = pendingSubs.get(src) ?? [];
    if (subs.length === 0) fetchQueue.push(src);
    subs.push(img);
    pendingSubs.set(src, subs);
    startNext();
  });
}
