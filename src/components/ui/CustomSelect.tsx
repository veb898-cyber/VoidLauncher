import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Tooltip } from './Tooltip';

export interface SelectOption<T extends string> {
  value: T;
  label: string;
  disabled?: boolean;
  description?: string;
  tooltip?: string;
}

interface CustomSelectProps<T extends string> {
  value: T;
  options: SelectOption<T>[];
  onChange: (value: T) => void;
  id?: string;
  className?: string;
  /** When true, opens upward (useful near the bottom of a modal). */
  openUpward?: boolean;
  /**
   * Disables the whole control: the trigger stops responding and the list
   * cannot be opened. Used when the value is temporarily not selectable
   * (e.g. normal themes while Liquid Glass is active).
   */
  disabled?: boolean;
}

const LIST_GAP = 4;
const LIST_MAX_H = 260;
/** Below this much room under the trigger, the list flips upward (when the
 *  space above is larger). Mirrors the old absolute placement's intent. */
const LIST_MIN_BELOW = 140;

interface ListPos {
  left: number;
  width: number;
  top: number | null;
  bottom: number | null;
  maxHeight: number;
}

/**
 * Fully themeable dropdown that replaces native <select>.
 * Required because WebView2/Chromium ignores `color-scheme: dark` for the
 * <option> popup on Windows, leaving white-on-white text.
 *
 * The open list is portaled to <body> and positioned `fixed` against its
 * trigger. Two reasons, both Liquid Glass: (1) an ancestor with
 * backdrop-filter forms a backdrop root, so an in-tree list only samples
 * that ancestor's own subtree — content outside (other sections, the page)
 * would stay sharp behind the glass; (2) sibling sections paint in tree
 * order and would cover a list that stays inside its own section. As a
 * body child the list samples the whole document and sits above every
 * page-level layer (z-index1600: above modal1000/modpacks1400/install1500,
 * below toasts2000 and tooltips3000).
 */
export function CustomSelect<T extends string>({
  value,
  options,
  onChange,
  id,
  className = '',
  openUpward = false,
  disabled = false,
}: CustomSelectProps<T>) {
  const [open, setOpen] = useState(false);
  const [focusIdx, setFocusIdx] = useState<number>(-1);
  const [pos, setPos] = useState<ListPos | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  const selected = options.find((o) => o.value === value);

  // Keep the portaled list glued to its trigger while anything scrolls.
  const place = useCallback(() => {
    const el = triggerRef.current;
    if (!el || !el.isConnected) {
      setOpen(false);
      return;
    }
    const r = el.getBoundingClientRect();
    // Trigger scrolled out of view: an orphaned list would be wrong here.
    if (r.bottom < 0 || r.top > window.innerHeight) {
      setOpen(false);
      return;
    }
    const below = window.innerHeight - r.bottom - LIST_GAP - 8;
    const above = r.top - LIST_GAP - 8;
    const up = openUpward || (below < LIST_MIN_BELOW && above > below);
    const avail = up ? above : below;
    setPos({
      left: r.left,
      width: r.width,
      top: up ? null : r.bottom + LIST_GAP,
      bottom: up ? window.innerHeight - r.top + LIST_GAP : null,
      maxHeight: Math.max(96, Math.min(LIST_MAX_H, avail)),
    });
  }, [openUpward]);

  useLayoutEffect(() => {
    if (!open) {
      setPos(null);
      return;
    }
    place();
    window.addEventListener('resize', place);
    document.addEventListener('scroll', place, true);
    return () => {
      window.removeEventListener('resize', place);
      document.removeEventListener('scroll', place, true);
    };
  }, [open, place]);

  // Close on outside click (the list lives outside wrapRef — check both)
  useEffect(() => {
    if (!open) return;
    const onDocMouseDown = (e: MouseEvent) => {
      const t = e.target as Node;
      const inWrap = wrapRef.current?.contains(t);
      const inList = listRef.current?.contains(t);
      if (!inWrap && !inList) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setOpen(false);
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setFocusIdx((i) => {
          let next = i < 0 ? 0 : i + 1;
          // skip disabled
          while (next < options.length && options[next].disabled) next++;
          if (next >= options.length) next = 0;
          return next;
        });
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        setFocusIdx((i) => {
          let next = i < 0 ? options.length - 1 : i - 1;
          while (next >= 0 && options[next].disabled) next--;
          if (next < 0) next = options.length - 1;
          return next;
        });
      } else if (e.key === 'Enter' || e.key === ' ') {
        if (focusIdx >= 0 && focusIdx < options.length && !options[focusIdx].disabled) {
          e.preventDefault();
          onChange(options[focusIdx].value);
          setOpen(false);
        }
      }
    };
    document.addEventListener('mousedown', onDocMouseDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDocMouseDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open, focusIdx, options, onChange]);

  // Scroll focused option into view
  useEffect(() => {
    if (open && focusIdx >= 0 && listRef.current) {
      const el = listRef.current.children[focusIdx] as HTMLElement | undefined;
      el?.scrollIntoView({ block: 'nearest' });
    }
  }, [focusIdx, open]);

  // Reset the highlight only when the list opens. Depending on `options` here
  // was wrong: callers build the array inline, so every parent re-render
  // produced a new identity, ran this effect and yanked the keyboard
  // highlight back to the selected item in the middle of ArrowUp/ArrowDown.
  const valueRef = useRef(value);
  const optionsRef = useRef(options);
  valueRef.current = value;
  optionsRef.current = options;
  useEffect(() => {
    if (open) {
      const idx = optionsRef.current.findIndex((o) => o.value === valueRef.current);
      setFocusIdx(idx >= 0 ? idx : 0);
    }
  }, [open]);

  // Becoming disabled while open would leave an unreachable popup on screen.
  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  const list = open && pos ? (
    <ul
      ref={listRef}
      className={`custom-select__list ${pos.top === null ? 'custom-select__list--up' : ''}`}
      role="listbox"
      style={{
        position: 'fixed',
        left: pos.left,
        right: 'auto',
        width: pos.width,
        top: pos.top ?? 'auto',
        bottom: pos.bottom ?? 'auto',
        maxHeight: pos.maxHeight,
      }}
    >
      {options.map((opt, i) => {
        const isSelected = opt.value === value;
        const isFocused = i === focusIdx;
        return (
          <li
            key={opt.value}
            role="option"
            aria-selected={isSelected}
            aria-disabled={opt.disabled}
            className={[
              'custom-select__option',
              isSelected && 'custom-select__option--selected',
              isFocused && 'custom-select__option--focused',
              opt.disabled && 'custom-select__option--disabled',
            ].filter(Boolean).join(' ')}
            onMouseEnter={() => setFocusIdx(i)}
            onClick={() => {
              if (opt.disabled) return;
              onChange(opt.value);
              setOpen(false);
            }}
          >
            {opt.tooltip ? (
              <Tooltip content={opt.tooltip}>
                <span className="custom-select__option-label">{opt.label}</span>
              </Tooltip>
            ) : (
              <span className="custom-select__option-label">{opt.label}</span>
            )}
            {opt.description && (
              <span className="custom-select__option-desc">{opt.description}</span>
            )}
            {isSelected && (
              <svg
                className="custom-select__check"
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="currentColor"
                aria-hidden="true"
              >
                <path d="M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41L9 16.17z" />
              </svg>
            )}
          </li>
        );
      })}
    </ul>
  ) : null;

  return (
    <div
      ref={wrapRef}
      className={`custom-select ${open ? 'custom-select--open' : ''} ${disabled ? 'custom-select--disabled' : ''} ${className}`.trim()}
    >
      <button
        type="button"
        id={id}
        ref={triggerRef}
        className="custom-select__trigger input"
        onClick={() => setOpen((o) => !o)}
        aria-haspopup="listbox"
        aria-expanded={open}
        disabled={disabled}
      >
        <span className="custom-select__value">{selected?.label ?? ''}</span>
        <svg
          className="custom-select__chevron"
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="currentColor"
          aria-hidden="true"
        >
          <path d="M7 10l5 5 5-5z" />
        </svg>
      </button>
      {createPortal(list, document.body)}
    </div>
  );
}
