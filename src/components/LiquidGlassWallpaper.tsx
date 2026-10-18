import { useEffect, useRef } from 'react';
import { useThemeStore } from '../stores/themeStore';
import { useWallpaperStore } from '../stores/wallpaperStore';
import { useFocusStore } from '../stores/focusStore';
import { getWallpaperScene, WALLPAPER_PERIOD } from '../lib/wallpapers';

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
 *  - painting is capped to ~30 fps (the scenes drift over 38-42 s cycles and
 *    the orbs over 90-130 s, so the cadence is invisible) while the clock
 *    keeps running on every rAF, so pausing and resuming never jumps time;
 *  - the glow orbs are painted into this same canvas instead of living as a
 *    CSS-animated layer: any animation running underneath the glass made the
 *    compositor rebuild every backdrop-filter surface on every display frame
 *    (165 Hz), on the main thread and the GPU alike, even while the window
 *    was minimised. The canvas clips them and rides the same frame budget;
 *  - backing store at DPR 1. The visible parts sit under a 22px blur and a
 *    0.68 fill, so device-pixel detail is never seen while costing 4x the fill
 *    rate on a HiDPI panel;
 *  - the loop stops entirely while the window is hidden, minimized or while a
 *    game is running unfocused, matching what `body.app-frozen` already does to
 *    the CSS animations. (A minimized WebView2 keeps `document.hidden` false,
 *    so minimize is tracked through the Tauri window API.)
 *
 * Pausing stops the clock and leaves the last frame on screen. Nothing here
 * ever clears the canvas: an empty substrate is a black hole exactly where the
 * material is supposed to be refracting something.
 */

/** Cap the clock step so a stalled tab cannot jump the scene forward. */
const MAX_DT = 0.1;
/** Paint cadence: 30 fps is indistinguishable on 40 s waves and 90-130 s orbs. */
const FRAME_INTERVAL_MS = 1000 / 30;
const TAU = Math.PI * 2;

/** The two light fields that were once `.lg-glow`: same colours, geometry and
 *  drift path and timing as the old CSS animation (`translate3d(16vw, 12vh)
 *  scale(1.18)`, 90 s alternate / 130 s alternate-reverse, ease-in-out). */
const GLOW_ORBS = [
  { hue: 'hsla(214, 100%, 66%, 0.30)', xFrac: -0.18, yFrac: -0.30, period: 90, reverse: false },
  { hue: 'hsla(186, 95%, 55%, 0.24)', xFrac: 1.24, yFrac: 1.34, period: 130, reverse: true },
] as const;
const GLOW_DRIFT_X = 0.16;
const GLOW_DRIFT_Y = 0.12;
const GLOW_SCALE = 1.18;

/** ease-in-out is visually a smoothstep over cycles this long. */
const smoothstep = (t: number) => t * t * (3 - 2 * t);

const orbPhase = (t: number, period: number, reverse: boolean) => {
  const cycle = (t / period) % 2;
  const triangle = cycle < 1 ? cycle : 2 - cycle;
  return smoothstep(reverse ? 1 - triangle : triangle);
};

/** Quarter-res buffer for the glow: the orbs are soft radial gradients, so a
 *  bilinear upscale is invisible under the glass while turning two full-canvas
 *  gradient passes per paint into one upscaled blit. */
const GLOW_DOWNSCALE = 4;
type GlowBuffer = { canvas: HTMLCanvasElement | null };

const paintGlow = (ctx: CanvasRenderingContext2D, width: number, height: number, t: number, buffer: GlowBuffer) => {
  const k = 1 / GLOW_DOWNSCALE;
  const gw = Math.max(1, Math.round(width * k));
  const gh = Math.max(1, Math.round(height * k));
  if (!buffer.canvas) buffer.canvas = document.createElement('canvas');
  const off = buffer.canvas;
  if (off.width !== gw || off.height !== gh) {
    off.width = gw;
    off.height = gh;
  }
  const gctx = off.getContext('2d');
  if (!gctx) return;
  const radius = Math.max(width, height) * 0.36;
  gctx.clearRect(0, 0, gw, gh);
  for (const orb of GLOW_ORBS) {
    const phase = orbPhase(t, orb.period, orb.reverse);
    const scale = 1 + (GLOW_SCALE - 1) * phase;
    const cx = (orb.xFrac * width + radius * (orb.xFrac < 0 ? 1 : -1) + GLOW_DRIFT_X * width * phase) * k;
    const cy = (orb.yFrac * height + radius * (orb.yFrac < 0 ? 1 : -1) + GLOW_DRIFT_Y * height * phase) * k;
    const r = radius * scale * k;
    const gradient = gctx.createRadialGradient(cx, cy, 0, cx, cy, r);
    gradient.addColorStop(0, orb.hue);
    gradient.addColorStop(0.7, 'hsla(0, 0%, 0%, 0)');
    gctx.fillStyle = gradient;
    gctx.fillRect(0, 0, gw, gh);
  }
  ctx.save();
  ctx.globalCompositeOperation = 'screen';
  ctx.drawImage(off, 0, 0, width, height);
  ctx.restore();
};

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

  const active = liquidGlass;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !active) return;

    const ctx = canvas.getContext('2d', { alpha: false });
    if (!ctx) return;

    const scene = getWallpaperScene(wallpaper);
    const period = WALLPAPER_PERIOD[wallpaper];
    const glowBuffer: GlowBuffer = { canvas: null };

    let width = 0;
    let height = 0;
    let last = 0;
    let lastPaint = 0;
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
        paintGlow(ctx, width, height, clockRef.current, glowBuffer);
      } finally {
        ctx.restore();
      }
    };

    const shouldRun = () =>
      !document.hidden &&
      !useFocusStore.getState().isFrozen &&
      !useFocusStore.getState().isMinimized &&
      !reducedMotionQuery()?.matches;

    const frame = (now: number) => {
      raf = requestAnimationFrame(frame);
      if (!last) last = now;
      // The clock advances on every rAF so wall-clock time stays true even
      // between paints; only the (expensive) repaint is gated.
      clockRef.current += Math.min((now - last) / 1000, MAX_DT);
      last = now;
      if (now - lastPaint < FRAME_INTERVAL_MS - 2) return;
      lastPaint = now;
      // A canvas that was just resized shows stale pixels, and the scene reads
      // w/h, so repaint regardless of whether the size changed.
      resize();
      paint();
    };

    const start = () => {
      if (raf) return;
      last = 0;
      lastPaint = 0;
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

    // `isFrozen`/`isMinimized` are deliberately not dependencies: the loop
    // reads them through the store and the effect is torn down and rebuilt
    // when the wallpaper changes, which is the only thing that should ever
    // rebuild the scene.
    const unsubFreeze = useFocusStore.subscribe((state, prev) => {
      if (state.isFrozen === prev.isFrozen && state.isMinimized === prev.isMinimized) return;
      if (shouldRun()) start();
      else stop();
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
  // orbs are painted into the canvas after the scene, so they melt into the
  // backdrop exactly like the old CSS glow layer did - and ride its clock.
  return (
    <div className="lg-substrate" aria-hidden="true">
      {active ? <canvas ref={canvasRef} className="lg-wallpaper" /> : null}
    </div>
  );
}
