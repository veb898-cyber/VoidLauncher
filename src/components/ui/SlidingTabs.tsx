import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from 'react';
import { Liquid } from 'liquid-gooey';
import { useGooey } from '../../hooks/useGooey';
import { useThemeStore } from '../../stores/themeStore';
import { adjacentEnabledTab, edgeEnabledTab, focusableTab, revealedScrollOffset } from './tabNavigation';

/** DOM id of one tab in a panel-controlled row. */
export function slidingTabId(prefix: string, id: string): string {
  return `${prefix}-tab-${id}`;
}

/** DOM id of the tabpanel controlled by one tab. */
export function slidingPanelId(prefix: string, id: string): string {
  return `${prefix}-panel-${id}`;
}


export interface SlidingTabDef<T extends string> {
  id: T;
  label: ReactNode;
  /** Tooltip / accessible name when the label is an abbreviation. */
  title?: string;
  /** Blocked while a background task (e.g. a scan) is in flight. */
  disabled?: boolean;
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
  controlsPanels = false,
  idPrefix,
}: {
  tabs: readonly SlidingTabDef<T>[];
  active: T;
  onChange: (id: T) => void;
  ariaLabel?: string;
  orientation?: 'horizontal' | 'vertical';
  /** Extra class on the row wrapper (e.g. compact density in the create dialog). */
  className?: string;
  style?: React.CSSProperties;
  /**
   * True for a real tab set whose tabs control tabpanels. The row gets
   * `role="tablist"` and tabs get `aria-controls` pointing at
   * `slidingPanelId(idPrefix, id)`. Filter/toggle rows leave this false and
   * are exposed as a labelled button group instead.
   */
  controlsPanels?: boolean;
  /** Stable prefix used by the tab/panel ids. Required when controlsPanels. */
  idPrefix?: string;
}) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const tabRefs = useRef(new Map<string, HTMLButtonElement>());
  const [thumb, setThumb] = useState<Thumb | null>(null);
  const generatedId = useId();
  const idBase = idPrefix ?? generatedId;
  const gooey = useGooey();
  // Liquid Glass paints the indicator with its own backdrop-filter; a goo
  // silhouette behind it would flatten the glass into an opaque blob. Base
  // themes keep the crisp gradient line and get the liquid trail.
  const liquidGlass = useThemeStore((s) => s.liquidGlass);
  const liquid = gooey && !liquidGlass;

  const vertical = orientation === 'vertical';
  const tabList = controlsPanels;
  const focusableId = focusableTab(tabs, active);

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

  // A tab row can be wider than its container (Settings at the minimum window
  // width, long translations, many scopes). Bring the active tab into view
  // without moving the page itself: only the row's own scroll offset changes.
  const revealActive = useCallback(() => {
    const wrap = wrapRef.current;
    const activeEl = tabRefs.current.get(active);
    if (!wrap || !activeEl) return;

    if (vertical) {
      const next = revealedScrollOffset(
        activeEl.offsetTop,
        activeEl.offsetHeight,
        wrap.scrollTop,
        wrap.clientHeight,
      );
      if (next !== wrap.scrollTop) wrap.scrollTop = next;
    } else {
      const next = revealedScrollOffset(
        activeEl.offsetLeft,
        activeEl.offsetWidth,
        wrap.scrollLeft,
        wrap.clientWidth,
      );
      if (next !== wrap.scrollLeft) wrap.scrollLeft = next;
    }
  }, [active, vertical]);

  const sync = useCallback(() => {
    measure();
    revealActive();
  }, [measure, revealActive]);

  // Measure after layout so the indicator is already in place on the very
  // first painted frame (the element is only mounted once measured, so its
  // first appearance never animates in from zero).
  useLayoutEffect(sync, [sync, tabs.length]);

  // Re-measure on window resize: the row can reflow with the container.
  useEffect(() => {
    window.addEventListener('resize', sync);
    return () => window.removeEventListener('resize', sync);
  }, [sync]);

  const onKeyDown = useCallback((event: KeyboardEvent<HTMLDivElement>) => {
    const origin = focusableId ?? active;
    const forward = vertical
      ? event.key === 'ArrowDown'
      : event.key === 'ArrowRight';
    const backward = vertical
      ? event.key === 'ArrowUp'
      : event.key === 'ArrowLeft';

    let next: T | null = null;
    if (forward) next = adjacentEnabledTab(tabs, origin, 1);
    else if (backward) next = adjacentEnabledTab(tabs, origin, -1);
    else if (event.key === 'Home') next = edgeEnabledTab(tabs, 'first');
    else if (event.key === 'End') next = edgeEnabledTab(tabs, 'last');
    else return;

    if (!next) return;
    event.preventDefault();
    tabRefs.current.get(next)?.focus();
    if (next !== active) onChange(next);
  }, [active, focusableId, onChange, tabs, vertical]);

  return (
    <div
      ref={wrapRef}
      className={`tabs${vertical ? ' tabs--vertical' : ''}${className ? ` ${className}` : ''}`}
      style={style}
      role={tabList ? 'tablist' : 'group'}
      aria-label={ariaLabel}
      aria-orientation={tabList ? (vertical ? 'vertical' : 'horizontal') : undefined}
      onKeyDown={onKeyDown}
    >
      {thumb && liquid ? (
        <Liquid
          fill="var(--primary)"
          blur={2}
          style={{ position: 'absolute', inset: 0, pointerEvents: 'none' }}
        >
          <Liquid.Item effect="move">
            <span
              className="tabs__indicator"
              style={{
                ['--tab-offset']: `${thumb.offset}px`,
                ['--tab-size']: `${thumb.size}px`,
              } as CSSProperties}
              aria-hidden
            />
          </Liquid.Item>
        </Liquid>
      ) : (
        thumb && (
          <span
            className="tabs__indicator"
            style={{
              ['--tab-offset']: `${thumb.offset}px`,
              ['--tab-size']: `${thumb.size}px`,
            } as CSSProperties}
            aria-hidden
          />
        )
      )}
      {tabs.map((tb) => (
        <button
          key={tb.id}
          type="button"
          id={tabList ? slidingTabId(idBase, tb.id) : undefined}
          role={tabList ? 'tab' : undefined}
          aria-selected={tabList ? active === tb.id : undefined}
          aria-pressed={tabList ? undefined : active === tb.id}
          aria-controls={tabList ? slidingPanelId(idBase, tb.id) : undefined}
          tabIndex={tb.id === focusableId ? 0 : -1}
          title={tb.title}
          ref={(el) => {
            if (el) tabRefs.current.set(tb.id, el);
            else tabRefs.current.delete(tb.id);
          }}
          className={`tab${active === tb.id ? ' tab--active' : ''}`}
          disabled={tb.disabled}
          onClick={() => onChange(tb.id)}
        >
          {tb.label}
        </button>
      ))}
    </div>
  );
}
