import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useT } from '../../lib/i18n';
import { renderMarkdownToHtml } from '../../lib/markdown';
import type { ModListPreviewKind } from '../../lib/modListExport';
import {
  injectPreviewTheme,
  readPreviewPalette,
  readStyleNonce,
  wrapPreviewBody,
  type PreviewPalette,
} from '../../lib/modListPreviewTheme';

interface ModListPreviewProps {
  content: string;
  kind: ModListPreviewKind;
}

const useSafeLayoutEffect =
  typeof window !== 'undefined' ? useLayoutEffect : useEffect;

/**
 * Source + rendered preview. The source is a real read-only textarea, so the
 * native selection, Ctrl+A and Ctrl+C behaviour is exactly the browser's; no
 * custom selection layer sits between the user and the text. The rendered
 * document lives in a sandboxed iframe without `allow-scripts`, which blocks
 * scripts even if a custom template contains one.
 *
 * The iframe document receives the launcher's live theme tokens. A
 * MutationObserver on the appearance attributes updates it when the user
 * switches between a normal theme and Liquid Glass without closing the dialog.
 */
export function ModListPreview({ content, kind }: ModListPreviewProps) {
  const t = useT();
  const sourceRef = useRef<HTMLTextAreaElement | null>(null);
  const selectionRef = useRef({ start: 0, end: 0 });
  const rendered = kind !== 'text';
  const [previewTheme, setPreviewTheme] = useState<{
    palette: PreviewPalette;
    nonce: string;
  }>(() => ({
    palette: readPreviewPalette(),
    nonce: readStyleNonce(),
  }));

  useEffect(() => {
    if (typeof document === 'undefined') return;
    const root = document.documentElement;
    const update = () =>
      setPreviewTheme({
        palette: readPreviewPalette(),
        nonce: readStyleNonce(),
      });
    const observer = new MutationObserver(update);
    observer.observe(root, {
      attributes: true,
      attributeFilter: [
        'data-theme',
        'data-lg-realistic',
        'data-font',
        'data-font-mono',
      ],
    });
    update();
    return () => observer.disconnect();
  }, []);

  const srcDoc = useMemo(() => {
    if (kind === 'html') {
      return injectPreviewTheme(
        content,
        previewTheme.palette,
        previewTheme.nonce,
      );
    }
    if (kind === 'markdown') {
      return wrapPreviewBody(
        renderMarkdownToHtml(content),
        previewTheme.palette,
        previewTheme.nonce,
      );
    }
    return '';
  }, [content, kind, previewTheme]);

  // A controlled textarea drops the browser selection when its value changes.
  // Restore the last selection only while the user is actually in the source
  // pane, so unrelated settings changes never move their cursor.
  useSafeLayoutEffect(() => {
    const element = sourceRef.current;
    if (!element || document.activeElement !== element) return;
    const start = Math.min(selectionRef.current.start, content.length);
    const end = Math.min(selectionRef.current.end, content.length);
    element.setSelectionRange(start, end);
  }, [content]);

  // Keep the remembered range in sync with mouse/keyboard selection.
  useEffect(() => {
    const element = sourceRef.current;
    if (!element) return;
    const remember = () => {
      selectionRef.current = {
        start: element.selectionStart,
        end: element.selectionEnd,
      };
    };
    element.addEventListener('select', remember);
    element.addEventListener('keyup', remember);
    element.addEventListener('mouseup', remember);
    return () => {
      element.removeEventListener('select', remember);
      element.removeEventListener('keyup', remember);
      element.removeEventListener('mouseup', remember);
    };
  }, []);

  return (
    <div
      className={`mod-export__preview${rendered ? '' : ' mod-export__preview--single'}`}
    >
      <div className="mod-export__pane">
        <textarea
          ref={sourceRef}
          className="input mod-export__source"
          readOnly
          value={content}
          wrap={kind === 'text' ? 'soft' : 'off'}
          spellCheck={false}
          aria-label={t('mod_export.preview_source')}
          onSelect={() => {
            const element = sourceRef.current;
            if (!element) return;
            selectionRef.current = {
              start: element.selectionStart,
              end: element.selectionEnd,
            };
          }}
        />
      </div>
      {rendered && (
        <div className="mod-export__pane">
          <iframe
            className="mod-export__frame"
            title={t('mod_export.preview_rendered')}
            sandbox=""
            referrerPolicy="no-referrer"
            srcDoc={srcDoc}
          />
        </div>
      )}
    </div>
  );
}
