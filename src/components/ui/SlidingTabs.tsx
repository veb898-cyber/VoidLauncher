import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';

export interface SlidingTabDef<T extends string> {
  id: T;
  label: ReactNode;
  /** Tooltip / accessible name when the label is an abbreviation. */
  title?: string;
}

interface Thumb {
  /** Distance from the row's origin along the slide axis (offsetLeft / offsetTop). */
  offset: number;
  /** Size along the slide axis (offsetWidth / offsetHeight). */
  size: number;
}

/**
 * Tab row whose active frame is one box that physically slides between the
 * buttons (measured from the real DOM like Segmented — labels differ per
 * language, so equal-width math would drift).
 *
 * Replaces the per-tab underline that only scaled in place and the pill that
 * popped in around the new tab: neither travels, and the switch read as a box
 * appearing "around" the new button instead of the frame moving to it (owner
 * directive: "рамочка... должна перемещаться плавно и красиво").
 *
 * `orientation="vertical"` slides the same frame down a column (create-menu
 * modes); the geometry travels through the `--tab-offset` / `--tab-size`
 * custom properties so every theme (including the glass pill inset) stays in
 * CSS and nothing is baked into inline px.
 */
export function SlidingTabs<T extends string>({
  tabs,
  active,
  onChange,
  ariaLabel,
  orientation = 'horizontal',
  className,
  style,
}: {
  tabs: readonly SlidingTabDef<T>[];
  active: T;
  onChange: (id: T) => void;
  ariaLabel?: string;
  orientation?: 'horizontal' | 'vertical';
  /** Extra class on the row wrapper (e.g. compact density in the create dialog). */
  className?: string;
  style?: React.CSSProperties;
}) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const tabRefs = useRef(new Map<string, HTMLButtonElement>());
  const [thumb, setThumb] = useState<Thumb | null>(null);

  const vertical = orientation === 'vertical';

  const measure = useCallback(() => {
    const activeEl = tabRefs.current.get(active);
    if (!activeEl) {
      setThumb(null);
      return;
    }
    setThumb(vertical
      ? { offset: activeEl.offsetTop, size: activeEl.offsetHeight }
      : { offset: activeEl.offsetLeft, size: activeEl.offsetWidth });
  }, [active, vertical]);

  // Measure after layout so the indicator is already in place on the very
  // first painted frame (the element is only mounted once measured, so its
  // first appearance never animates in from zero).
  useLayoutEffect(measure, [measure, tabs.length]);

  // Re-measure on window resize: the row can reflow with the container.
  useEffect(() => {
    window.addEventListener('resize', measure);
    return () => window.removeEventListener('resize', measure);
  }, [measure]);

  return (
    <div
      ref={wrapRef}
      className={`tabs${vertical ? ' tabs--vertical' : ''}${className ? ` ${className}` : ''}`}
      style={style}
      role="tablist"
      aria-label={ariaLabel}
    >
      {thumb && (
        <span
          className="tabs__indicator"
          style={{
            ['--tab-offset']: `${thumb.offset}px`,
            ['--tab-size']: `${thumb.size}px`,
          } as CSSProperties}
          aria-hidden
        />
      )}
      {tabs.map((tb) => (
        <button
          key={tb.id}
          type="button"
          role="tab"
          aria-selected={active === tb.id}
          title={tb.title}
          ref={(el) => {
            if (el) tabRefs.current.set(tb.id, el);
            else tabRefs.current.delete(tb.id);
          }}
          className={`tab${active === tb.id ? ' tab--active' : ''}`}
          onClick={() => onChange(tb.id)}
        >
          {tb.label}
        </button>
      ))}
    </div>
  );
}
