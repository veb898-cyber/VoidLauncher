import { useEffect, useRef, useState } from 'react';

export interface SelectOption<T extends string> {
  value: T;
  label: string;
  disabled?: boolean;
  description?: string;
}

interface CustomSelectProps<T extends string> {
  value: T;
  options: SelectOption<T>[];
  onChange: (value: T) => void;
  id?: string;
  className?: string;
  /** When true, opens upward (useful near the bottom of a modal). */
  openUpward?: boolean;
}

/**
 * Fully themeable dropdown that replaces native <select>.
 * Required because WebView2/Chromium ignores `color-scheme: dark` for the
 * <option> popup on Windows, leaving white-on-white text.
 */
export function CustomSelect<T extends string>({
  value,
  options,
  onChange,
  id,
  className = '',
  openUpward = false,
}: CustomSelectProps<T>) {
  const [open, setOpen] = useState(false);
  const [focusIdx, setFocusIdx] = useState<number>(-1);
  const wrapRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  const selected = options.find((o) => o.value === value);

  // Close on outside click
  useEffect(() => {
    if (!open) return;
    const onDocMouseDown = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
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

  // Reset focus index when opening
  useEffect(() => {
    if (open) {
      const idx = options.findIndex((o) => o.value === value);
      setFocusIdx(idx >= 0 ? idx : 0);
    }
  }, [open, value, options]);

  return (
    <div
      ref={wrapRef}
      className={`custom-select ${open ? 'custom-select--open' : ''} ${className}`.trim()}
    >
      <button
        type="button"
        id={id}
        className="custom-select__trigger input"
        onClick={() => setOpen((o) => !o)}
        aria-haspopup="listbox"
        aria-expanded={open}
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
      {open && (
        <ul
          ref={listRef}
          className={`custom-select__list ${openUpward ? 'custom-select__list--up' : ''}`}
          role="listbox"
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
                <span className="custom-select__option-label">{opt.label}</span>
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
      )}
    </div>
  );
}
