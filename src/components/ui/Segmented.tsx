import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
export interface SegmentedOption<T extends string> {
  value: T;
  label: ReactNode;
  /** Tooltip / accessible name when the label is an abbreviation. */
  title?: string;
  disabled?: boolean;
}

export interface SegmentedProps<T extends string> {
  options: readonly SegmentedOption<T>[];
  ariaLabel: string;
  /** Stretch to the full width of the parent (never overflow it). */
  block?: boolean;
  className?: string;
  style?: React.CSSProperties;
  /**
   * Single choice: the active option is marked with a filled indicator that
   * slides to the picked option.
   */
  value?: T;
  onChange?: (value: T) => void;
  /**
   * Multi choice (independent filters): every option carries its own pressed
   * state, so no moving indicator is used.
   */
  pressed?: Record<string, boolean>;
  onToggle?: (value: T) => void;
}

interface Thumb {
  left: number;
  width: number;
}

/**
 * Segmented control with a sliding indicator for single-choice groups.
 *
 * The indicator is measured from the real DOM (offsetLeft/offsetWidth) instead
 * of being computed from an assumed item width — labels differ per language
 * ("Все"/"Snapshots"), so anything derived from equal-width math drifts.
 */
export function Segmented<T extends string>({
  options,
  ariaLabel,
  block,
  className,
  style,
  value,
  onChange,
  pressed,
  onToggle,
}: SegmentedProps<T>) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const optionRefs = useRef(new Map<string, HTMLButtonElement>());
  const [thumb, setThumb] = useState<Thumb | null>(null);
  const single = onChange != null && value != null;

  const measure = useCallback(() => {
    if (!single) {
      setThumb(null);
      return;
    }
    const wrap = wrapRef.current;
    const active = optionRefs.current.get(value);
    if (!wrap || !active) {
      setThumb(null);
      return;
    }
    setThumb({ left: active.offsetLeft, width: active.offsetWidth });
  }, [single, value]);

  // Measure after layout so the indicator is already in place on the very
  // first painted frame of a re-render (no flash of an unstyled control).
  useLayoutEffect(measure, [measure, options.length]);

  // Re-measure on window resize: block mode makes the items fluid, so their
  // positions move with the container.
  useEffect(() => {
    if (!single) return;
    window.addEventListener('resize', measure);
    return () => window.removeEventListener('resize', measure);
  }, [single, measure]);

  return (
    <div
      ref={wrapRef}
      className={['segmented', block ? 'segmented--block' : null, className].filter(Boolean).join(' ')}
      style={style}
      role="group"
      aria-label={ariaLabel}
    >
      {single && thumb && (
        <span
          className="segmented__indicator"
          style={{ width: thumb.width, transform: `translateX(${thumb.left}px)` }}
          aria-hidden
        />
      )}
      {options.map((opt) => {
        const isActive = single ? value === opt.value : !!pressed?.[opt.value];
        return (
          <button
            key={opt.value}
            type="button"
            ref={(el) => {
              if (el) optionRefs.current.set(opt.value, el);
              else optionRefs.current.delete(opt.value);
            }}
            className={`segmented__option${isActive ? ` segmented__option--${single ? 'active' : 'on'}` : ''}`}
            aria-pressed={isActive}
            title={opt.title}
            disabled={opt.disabled}
            onClick={() => (single ? onChange?.(opt.value) : onToggle?.(opt.value))}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}
