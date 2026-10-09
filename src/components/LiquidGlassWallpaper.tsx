import { useEffect, useRef } from 'react';
import { useThemeStore } from '../stores/themeStore';
import { useWallpaperStore } from '../stores/wallpaperStore';
import { useFocusStore } from '../stores/focusStore';
import { getWallpaperScene, isAnimatedWallpaper, WALLPAPER_PERIOD } from '../lib/wallpapers';

/**
 * Animated backdrop for the Liquid Glass appearance.
 *
 * The substrate is the field the glass actually refracts. The content pane is
 * transparent, so this shows raw on empty space and is sampled - blurred and
 * tinted - by every glass surface on top of it: rail, cards, menus, sheets.
 * That is the whole point: it gives the blur something to blur. (A glass layer
 * over the content area was tried and removed on the owner's order: a
 * permanent fill + blur turned the wallpapers into grey mush.)
 *
 * Cost control, since this runs for as long as the launcher is open:
 *  - one rAF for the whole app, not one per scene (there is only ever one);
 *  - backing store at DPR 1. The visible parts sit under a 22px blur and a
 *    0.68 fill, so device-pixel detail is never seen while costing 4x the fill
 *    rate on a HiDPI panel;
 *  - the loop stops entirely while the window is hidden and while a game is
 *    running unfocused, matching what `body.app-frozen` already does to the
 *    CSS animations (the glow layer included).
 *
 * Pausing stops the clock and leaves the last frame on screen. Nothing here
 * ever clears the canvas: an empty substrate is a black hole exactly where the
 * material is supposed to be refracting something.
 */

/** Cap the clock step so a stalled tab cannot jump the scene forward. */
const MAX_DT = 0.1;
const TAU = Math.PI * 2;

function reducedMotionQuery(): MediaQueryList | null {
  return typeof window.matchMedia === 'function'
    ? window.matchMedia('(prefers-reduced-motion: reduce)')
    : null;
}

export default function LiquidGlassWallpaper() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // Kept in a ref so freezing and unfreezing never restarts the animation: a
  // clock that reset to zero on every focus change would visibly jump.
  const clockRef = useRef(0);

  const liquidGlass = useThemeStore((s) => s.liquidGlass);
  const wallpaper = useWallpaperStore((s) => s.wallpaper);

  const active = liquidGlass && isAnimatedWallpaper(wallpaper);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !active || !isAnimatedWallpaper(wallpaper)) return;

    const ctx = canvas.getContext('2d', { alpha: false });
    if (!ctx) return;

    const scene = getWallpaperScene(wallpaper);
    const period = WALLPAPER_PERIOD[wallpaper];

    let width = 0;
    let height = 0;
    let last = 0;
    let raf = 0;

    const resize = () => {
      const w = Math.max(1, Math.round(window.innerWidth));
      const h = Math.max(1, Math.round(window.innerHeight));
      if (w === width && h === height) return false;
      width = w;
      height = h;
      canvas.width = width;
      canvas.height = height;
      return true;
    };

    const paint = () => {
      ctx.save();
      try {
        scene.draw(ctx, width, height, ((clockRef.current / period) % 1) * TAU);
      } finally {
        ctx.restore();
      }
    };

    const shouldRun = () =>
      !document.hidden &&
      !useFocusStore.getState().isFrozen &&
      !reducedMotionQuery()?.matches;

    const frame = (now: number) => {
      raf = requestAnimationFrame(frame);
      if (!last) last = now;
      clockRef.current += Math.min((now - last) / 1000, MAX_DT);
      last = now;
      // A canvas that was just resized shows stale pixels, and the scene reads
      // w/h, so repaint regardless of whether the size changed.
      resize();
      paint();
    };

    const start = () => {
      if (raf) return;
      last = 0;
      raf = requestAnimationFrame(frame);
    };

    const stop = () => {
      if (!raf) return;
      cancelAnimationFrame(raf);
      raf = 0;
    };

    resize();
    paint();
    if (shouldRun()) start();

    const onResize = () => {
      if (resize()) paint();
    };
    const onVisibility = () => {
      if (document.hidden) stop();
      else if (shouldRun()) start();
    };
    const onMotionPreference = () => {
      if (reducedMotionQuery()?.matches) stop();
      else if (shouldRun()) start();
    };

    window.addEventListener('resize', onResize);
    document.addEventListener('visibilitychange', onVisibility);
    const motion = reducedMotionQuery();
    motion?.addEventListener?.('change', onMotionPreference);

    // `isFrozen` is deliberately not a dependency: the loop reads it through
    // the store and the effect is torn down and rebuilt when the wallpaper
    // changes, which is the only thing that should ever rebuild the scene.
    const unsubFreeze = useFocusStore.subscribe((state, prev) => {
      if (state.isFrozen === prev.isFrozen) return;
      if (state.isFrozen) stop();
      else if (shouldRun()) start();
    });

    return () => {
      stop();
      window.removeEventListener('resize', onResize);
      document.removeEventListener('visibilitychange', onVisibility);
      motion?.removeEventListener?.('change', onMotionPreference);
      unsubFreeze();
    };
  }, [active, wallpaper]);

  // The canvas has to be a CHILD of the substrate, not a sibling: the substrate
  // paints an opaque base colour, so a sibling would sit behind it and never be
  // seen. As a child it covers that base (the context is opaque), and the
  // substrate's ::before/::after washes are lifted above it in CSS. The glow
  // layer sits above the canvas as well: it is screen-blended light, so it has
  // to be painted after the scene it lights up.
  return (
    <div className="lg-substrate" aria-hidden="true">
      {active ? <canvas ref={canvasRef} className="lg-wallpaper" /> : null}
      <div className="lg-glow">
        <span className="lg-glow__orb lg-glow__orb--a" />
        <span className="lg-glow__orb lg-glow__orb--b" />
      </div>
    </div>
  );
}
