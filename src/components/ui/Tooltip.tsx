import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';

interface TooltipProps {
  content: ReactNode;
  children: ReactNode;
  /** Which side of the trigger the bubble appears on. Defaults to right. */
  side?: 'top' | 'bottom' | 'left' | 'right';
  /** Delay before showing, ms. Defaults to 700. */
  delay?: number;
}

const GAP = 8;

/**
 * Hover hints must not flash while the cursor travels across a row of
 * buttons. Windows/Chromium show native tooltips after ~1 s; the previous
 * 150 ms made every pass over the sidebar pop a label, so the hint now
 * appears only when the user actually rests on a control.
 */
const DEFAULT_DELAY_MS = 700;

/**
 * Lightweight, theme-aware hover hint. Inherits `--font-ui` and the active
 * theme; the bubble is rendered through a portal so it is never clipped by
 * ancestors with overflow. Opens on hover/focus, closes on leave/blur/Escape.
 */
export function Tooltip({ content, children, side = 'right', delay = DEFAULT_DELAY_MS }: TooltipProps) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState({ top: 0, left: 0 });
  const triggerRef = useRef<HTMLSpanElement>(null);
  const bubbleRef = useRef<HTMLDivElement>(null);
  const timerRef = useRef<number | null>(null);

  const clearTimer = useCallback(() => {
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  // Position the bubble next to the trigger, clamped to the viewport.
  const place = useCallback(() => {
    const trigger = triggerRef.current;
    const bubble = bubbleRef.current;
    if (!trigger || !bubble) return;
    // The wrapper span uses `display: contents` (no box), so measure the
    // actual child element; otherwise getBoundingClientRect() returns zeros
    // and the bubble would render in the top-left corner of the window.
    const anchor = trigger.firstElementChild || trigger;
    const rect = anchor.getBoundingClientRect();
    const bw = bubble.offsetWidth;
    const bh = bubble.offsetHeight;
    const vw = window.innerWidth;
    const vh = window.innerHeight;

    let top: number;
    let left: number;

    if (side === 'left' || side === 'right') {
      left = side === 'right' ? rect.right + GAP : rect.left - GAP - bw;
      // Flip to the other horizontal side when there is no room.
      if (side === 'right' && left + bw > vw - GAP) left = rect.left - GAP - bw;
      if (side === 'left' && left < GAP) left = rect.right + GAP;
      left = Math.max(GAP, Math.min(left, vw - bw - GAP));
      top = Math.max(GAP, Math.min(rect.top + rect.height / 2 - bh / 2, vh - bh - GAP));
    } else {
      left = Math.max(GAP, Math.min(rect.left + rect.width / 2 - bw / 2, vw - bw - GAP));
      top = side === 'top' ? rect.top - bh - GAP : rect.bottom + GAP;
      if (top < GAP) top = rect.bottom + GAP;
      top = Math.max(GAP, Math.min(top, vh - bh - GAP));
    }

    setPos({ top, left });
  }, [side]);

  const openTip = useCallback(() => {
    clearTimer();
    timerRef.current = window.setTimeout(() => {
      setOpen(true);
      place();
    }, delay);
  }, [clearTimer, delay, place]);

  const closeTip = useCallback(() => {
    // No grace period: the moment the cursor leaves the trigger (or the
    // other open-state sources of the effect fire) the bubble starts
    // fading — owner: «убиралась чуть быстрее».
    clearTimer();
    setOpen(false);
  }, [clearTimer]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeTip();
    };
    const onResize = () => place();
    // Chromium does not fire `mouseleave` when the trigger scrolls out from
    // under a stationary cursor, is disabled or replaced while hovered, or
    // when the window loses focus — the bubble would stick until the next
    // click. Close on any scroll (capture: also container scrolls), on
    // pointer movement outside the trigger, on window blur, and via a light
    // watchdog for the remaining "no events at all" cases.
    const onScroll = () => closeTip();
    const onPointerMove = (e: PointerEvent) => {
      const target = e.target as Node | null;
      if (target && triggerRef.current?.contains(target)) return;
      closeTip();
    };
    const onBlur = () => closeTip();
    const watchdog = window.setInterval(() => {
      const trigger = triggerRef.current;
      const anchor = (trigger?.firstElementChild as HTMLElement | null) ?? trigger;
      if (!anchor || !anchor.isConnected) {
        closeTip();
        return;
      }
      const active = document.activeElement;
      const focused = !!active && (anchor === active || anchor.contains(active));
      if (!focused && !anchor.matches(':hover')) closeTip();
    }, 500);
    window.addEventListener('keydown', onKey);
    window.addEventListener('resize', onResize);
    window.addEventListener('scroll', onScroll, true);
    window.addEventListener('pointermove', onPointerMove, true);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('resize', onResize);
      window.removeEventListener('scroll', onScroll, true);
      window.removeEventListener('pointermove', onPointerMove, true);
      window.removeEventListener('blur', onBlur);
      window.clearInterval(watchdog);
    };
  }, [open, closeTip, place]);

  useEffect(() => clearTimer, [clearTimer]);

  return (
    <>
      <span
        ref={triggerRef}
        className="tooltip-trigger"
        onMouseEnter={openTip}
        onMouseLeave={closeTip}
        onFocus={openTip}
        onBlur={closeTip}
      >
        {children}
      </span>
      {createPortal(
        <div
          ref={bubbleRef}
          className={`tooltip-bubble${open ? ' tooltip-bubble--visible' : ''}`}
          style={{ top: pos.top, left: pos.left, visibility: open ? 'visible' : 'hidden' }}
          role={open ? 'tooltip' : undefined}
        >
          {content}
        </div>,
        document.body,
      )}
    </>
  );
}