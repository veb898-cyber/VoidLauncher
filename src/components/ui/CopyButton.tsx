import { useCallback, useEffect, useRef, useState, type CSSProperties } from 'react';
import { Check, Copy } from 'lucide-react';
import { addToast } from './Toast';
import { t } from '../../lib/i18n';

const COPIED_VISIBLE_MS = 1400;

interface CopyButtonProps {
  /** Produces the text at click time (kept lazy so large logs are not
   *  joined unless the user actually copies). */
  text: () => string | Promise<string>;
  /** Tooltip and aria-label — the visible label, when present, is `label`. */
  tooltip: string;
  size?: number;
  /** Optional visible label rendered after the icon (analysis panel). */
  label?: string;
  className?: string;
  style?: CSSProperties;
  disabled?: boolean;
}

/**
 * Button with copy feedback built in: the Copy icon flips to a checkmark in
 * place (no toast), then softly flips back. Failures still surface as a
 * toast — there is no visual place for them on the button itself.
 */
export function CopyButton({
  text,
  tooltip,
  size = 15,
  label,
  className = 'btn btn--secondary btn--sm',
  style,
  disabled,
}: CopyButtonProps) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    },
    [],
  );

  const handleClick = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(await text());
      setCopied(true);
      if (timer.current !== null) window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => {
        timer.current = null;
        setCopied(false);
      }, COPIED_VISIBLE_MS);
    } catch {
      addToast(t('common.copy_failed'), 'error');
    }
  }, [text]);

  return (
    <button
      type="button"
      className={className}
      style={style}
      aria-label={tooltip}
      disabled={disabled}
      onClick={handleClick}
    >
      <span
        className={`copy-flip${copied ? ' copy-flip--done' : ''}`}
        style={{ width: size, height: size }}
        aria-hidden
      >
        <Copy size={size} className="copy-flip__idle" />
        <Check size={size} className="copy-flip__done" />
      </span>
      {label && <span style={{ marginLeft: 6 }}>{label}</span>}
    </button>
  );
}
