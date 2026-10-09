/**
 * Animated backdrops for the Liquid Glass appearance.
 *
 * These are four of the seventeen backgrounds from the isolated lab
 * (`docs/liquid-glass-background-lab/scenes.js`), ported here unchanged in
 * design and verbatim in their maths so the lab stays a valid measuring
 * instrument: if a number is retuned, it has to be retuned in both places or
 * the lab stops predicting what the app will look like.
 *
 * Two rules carried over from the lab, both load-bearing:
 *
 *  1. Every time-dependent term is `sin`/`cos` of an INTEGER multiple of the
 *     phase, so phase 0 and phase TAU produce identical pixels and the loop
 *     never jumps on restart. Retuning any frequency to a fractional value
 *     breaks this guarantee.
 *  2. Pausing must FREEZE a frame, never blank it. The substrate is the only
 *     thing behind the glass surfaces, so a blank canvas is a black hole where
 *     the material should be refracting something.
 *
 * The lab's own note applies here: `getImageData` cannot read a composited
 * `backdrop-filter`, so none of this can be verified by reading pixels back.
 * The scenes are correct because they are the same code the lab measures.
 */

const TAU = Math.PI * 2;

export type WallpaperId = 'none' | 'waves' | 'ribbons' | 'mesh' | 'nebula';

export const WALLPAPER_IDS: readonly WallpaperId[] = [
  'none',
  'waves',
  'ribbons',
  'mesh',
  'nebula',
];

/** Seconds for one full cycle. Must be a whole number of texture revolutions. */
export const WALLPAPER_PERIOD: Record<Exclude<WallpaperId, 'none'>, number> = {
  waves: 40,
  ribbons: 38,
  mesh: 38,
  nebula: 42,
};

export interface WallpaperScene {
  draw(ctx: CanvasRenderingContext2D, w: number, h: number, phase: number): void;
}

/* ------------------------------------------------------------------ helpers */

/** Deterministic rng, so the backdrop looks identical on every launch. */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

interface Wash {
  hue: number;
  a: number;
  r: number;
  sx: number;
  sy: number;
  fx: number;
  fy: number;
  px: number;
  py: number;
  sat?: number;
  light?: number;
}

interface AtmosphereOpts {
  hue?: number;
  sat?: number;
  lo?: number;
  hi?: number;
  washes?: Wash[];
}

/**
 * A material can only show what is underneath it. A backdrop that leaves a
 * third of the frame near-black gives the glass nothing to work with, and no
 * amount of blur tuning fixes that — so every scene starts from a slowly
 * shifting low-frequency base plus a few very large, very soft washes.
 *
 * Deliberately the lowest possible spatial frequency: broad, slow, no detail,
 * so the substrate never competes with the content for attention. The first
 * fill is fully opaque, which is also why no scene needs to clear the canvas.
 */
function atmosphere(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  p: number,
  o: AtmosphereOpts,
): void {
  const diag = Math.sqrt(w * w + h * h);
  const hue = (o.hue ?? 224) + 14 * Math.sin(p);
  const sat = o.sat ?? 28;
  const lo = o.lo ?? 0.07;
  const hi = o.hi ?? 0.12;

  const g = ctx.createLinearGradient(0, 0, w * 0.3, h);
  g.addColorStop(0.0, `hsl(${(hue + 10) % 360},${sat}%,${(hi * 100).toFixed(1)}%)`);
  g.addColorStop(0.55, `hsl(${hue},${sat}%,${((hi + lo) * 50).toFixed(1)}%)`);
  g.addColorStop(1.0, `hsl(${(hue + 18) % 360},${sat + 4}%,${(lo * 100).toFixed(1)}%)`);
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);

  const washes = o.washes ?? [];
  ctx.globalCompositeOperation = 'lighter';
  for (let i = 0; i < washes.length; i++) {
    const b = washes[i];
    const x = w * (0.5 + b.sx * Math.sin(p * b.fx + b.px));
    const y = h * (0.5 + b.sy * Math.sin(p * b.fy + b.py));
    const r = diag * b.r;
    const wh = (b.hue + 20 * Math.sin(p + b.px) + 360) % 360;
    const s = b.sat ?? 60;
    const l = b.light ?? 54;
    const rg = ctx.createRadialGradient(x, y, 0, x, y, r);
    rg.addColorStop(0.0, `hsla(${wh},${s}%,${l}%,${b.a})`);
    rg.addColorStop(0.45, `hsla(${wh},${s}%,${l}%,${b.a * 0.42})`);
    rg.addColorStop(0.78, `hsla(${wh},${s}%,${l}%,${b.a * 0.1})`);
    rg.addColorStop(1.0, 'hsla(0,0%,0%,0)');
    ctx.fillStyle = rg;
    ctx.fillRect(0, 0, w, h);
  }
  ctx.globalCompositeOperation = 'source-over';
}

/* -------------------------------------------------------------------- waves */

function createWaves(): WallpaperScene {
  return {
    draw(ctx, w, h, p) {
      atmosphere(ctx, w, h, p, {
        hue: 220,
        sat: 28,
        lo: 0.055,
        hi: 0.09,
        washes: [
          { hue: 194, a: 0.09, r: 0.9, sx: 0.24, sy: 0.2, fx: 1, fy: 1, px: 0.8, py: 1.4 },
          { hue: 300, a: 0.075, r: 0.86, sx: 0.22, sy: 0.22, fx: 1, fy: 1, px: 2.7, py: 0.2 },
        ],
      });

      ctx.globalCompositeOperation = 'lighter';
      const bands = 8;
      for (let i = 0; i < bands; i++) {
        const t = i / (bands - 1);
        const rate = 1 + Math.round(t * 2);
        const amp = h * (0.05 + 0.1 * Math.sin(t * Math.PI));
        const baseY = h * (0.08 + 0.84 * t);
        const drift = p * rate;
        const hue = (196 + i * 22 + 40 * Math.sin(p + i)) % 360;
        const g = ctx.createLinearGradient(0, baseY - amp * 2, 0, baseY + amp * 3);
        g.addColorStop(0.0, `hsla(${hue},96%,64%,0)`);
        g.addColorStop(0.22, `hsla(${hue},96%,62%,0.46)`);
        g.addColorStop(0.55, `hsla(${(hue + 34) % 360},92%,52%,0.26)`);
        g.addColorStop(1.0, `hsla(${(hue + 60) % 360},88%,44%,0)`);
        ctx.fillStyle = g;
        ctx.beginPath();
        ctx.moveTo(0, h + 4);
        const steps = 64;
        for (let s = 0; s <= steps; s++) {
          const u = s / steps;
          const y =
            baseY +
            Math.sin(u * 6.0 + drift) * amp +
            Math.sin(u * 2.3 - drift * 2) * amp * 0.6 +
            Math.sin(u * 11.0 + drift * 3) * amp * 0.18;
          ctx.lineTo(u * w, y);
        }
        ctx.lineTo(w, h + 4);
        ctx.closePath();
        ctx.fill();
      }
      ctx.globalCompositeOperation = 'source-over';
    },
  };
}

/* ------------------------------------------------------------------ ribbons */

function createRibbons(): WallpaperScene {
  const N = 110;
  // Lab allocated two arrays of 111 [x, y] pairs per ribbon per frame — over a
  // thousand short-lived arrays every frame, forever. Same maths, flat reuse.
  const top = new Float32Array((N + 1) * 2);
  const bot = new Float32Array((N + 1) * 2);

  return {
    draw(ctx, w, h, p) {
      atmosphere(ctx, w, h, p, {
        hue: 232,
        sat: 30,
        lo: 0.055,
        hi: 0.09,
        washes: [
          { hue: 206, a: 0.1, r: 0.9, sx: 0.24, sy: 0.18, fx: 1, fy: 1, px: 1.2, py: 0.6 },
          { hue: 284, a: 0.075, r: 0.84, sx: 0.22, sy: 0.24, fx: 1, fy: 1, px: 3.1, py: 1.9 },
        ],
      });

      const count = 5;
      for (let k = 0; k < count; k++) {
        const rate = k + 1;
        const baseY = h * (0.18 + 0.16 * k);
        const hue = (192 + k * 54 + 28 * Math.sin(p + k)) % 360;
        const half = h * (0.072 + 0.03 * Math.sin(p * 2 + k));

        // A wide soft glow under each ribbon, so it reads as a luminous band
        // rather than an isolated stroke on black.
        ctx.globalCompositeOperation = 'lighter';
        const gw = ctx.createLinearGradient(0, baseY - half * 5, 0, baseY + half * 5);
        gw.addColorStop(0.0, `hsla(${hue},90%,60%,0)`);
        gw.addColorStop(0.5, `hsla(${hue},90%,58%,0.1)`);
        gw.addColorStop(1.0, `hsla(${hue},90%,60%,0)`);
        ctx.fillStyle = gw;
        ctx.fillRect(0, baseY - half * 5, w, half * 10);

        for (let i = 0; i <= N; i++) {
          const u = i / N;
          const wob =
            Math.sin(u * 5.1 + p * rate + k) * h * 0.1 +
            Math.sin(u * 10.7 - p * 2 + k) * h * 0.042 +
            Math.sin(u * 2.2 + p) * h * 0.065;
          const th = half * (0.5 + 0.5 * Math.sin(u * 6.6 - p * 2 + k));
          top[i * 2] = u * w;
          top[i * 2 + 1] = baseY + wob - th;
          bot[i * 2] = u * w;
          bot[i * 2 + 1] = baseY + wob + th;
        }

          const g = ctx.createLinearGradient(0, 0, w, 0);
          g.addColorStop(0.0, `hsla(${hue},94%,66%,0)`);
          g.addColorStop(0.22, `hsla(${hue},94%,64%,0.54)`);
          g.addColorStop(0.5, `hsla(${(hue + 30) % 360},92%,58%,0.42)`);
          g.addColorStop(0.8, `hsla(${(hue + 52) % 360},90%,60%,0.47)`);
          g.addColorStop(1.0, `hsla(${(hue + 52) % 360},90%,60%,0)`);
        ctx.globalCompositeOperation = 'source-over';
        ctx.fillStyle = g;
        ctx.beginPath();
        ctx.moveTo(top[0], top[1]);
        for (let a = 1; a <= N; a++) ctx.lineTo(top[a * 2], top[a * 2 + 1]);
        for (let b = N; b >= 0; b--) ctx.lineTo(bot[b * 2], bot[b * 2 + 1]);
        ctx.closePath();
        ctx.fill();

        // Thin specular core, so narrow highlights exist to refract. Kept
        // deliberately dim: a near-white line blooms under blur and was the
        // hottest pixel in the lab.
        ctx.globalCompositeOperation = 'lighter';
        ctx.strokeStyle = `hsla(${(hue + 20) % 360},100%,82%,0.16)`;
        ctx.lineWidth = Math.max(1, h * 0.005);
        ctx.beginPath();
        for (let c = 0; c <= N; c++) {
          const mx = (top[c * 2] + bot[c * 2]) * 0.5;
          const my = (top[c * 2 + 1] + bot[c * 2 + 1]) * 0.5;
          if (c === 0) ctx.moveTo(mx, my);
          else ctx.lineTo(mx, my);
        }
        ctx.stroke();
        ctx.globalCompositeOperation = 'source-over';
      }
    },
  };
}

/* --------------------------------------------------------------------- mesh */

const MESH_HUES = [206, 268, 322, 38, 168];

/** Field pixel budget, matching the lab's 60x40. */
const MESH_FIELD_PIXELS = 2400;

interface MeshNode {
  gx: number;
  gy: number;
  hue: number;
  ax: number;
  ay: number;
  fx: number;
  fy: number;
  px: number;
  py: number;
  light: number;
}

function createMesh(): WallpaperScene {
  const COLS = 4;
  const ROWS = 3;
  const rnd = mulberry32(0x9e31);
  const nodes: MeshNode[] = [];
  for (let r = 0; r < ROWS; r++) {
    for (let c = 0; c < COLS; c++) {
      nodes.push({
        gx: c / (COLS - 1),
        gy: r / (ROWS - 1),
        hue: MESH_HUES[(r * COLS + c) % MESH_HUES.length],
        ax: 0.08 + rnd() * 0.1,
        ay: 0.07 + rnd() * 0.1,
        fx: 1 + Math.floor(rnd() * 2),
        fy: 1 + Math.floor(rnd() * 2),
        px: rnd() * TAU,
        py: rnd() * TAU,
        light: 0.42 + rnd() * 0.24,
      });
    }
  }

  // Per-node colour is constant across the whole field, so it is resolved once
  // per frame instead of once per field pixel. The lab called hsv2rgb 28,800
  // times a frame for values that do not depend on x or y.
  const nodeR = new Float32Array(nodes.length);
  const nodeG = new Float32Array(nodes.length);
  const nodeB = new Float32Array(nodes.length);
  const nodeX = new Float32Array(nodes.length);
  const nodeY = new Float32Array(nodes.length);
  const nodeHue = new Float32Array(nodes.length);

  let bw = 0;
  let bh = 0;
  let buf: { canvas: HTMLCanvasElement; ctx: CanvasRenderingContext2D } | null = null;
  let img: ImageData | null = null;

  /**
   * The lab hard-coded a 60x40 field, which is 3:2. A full window is rarely
   * 3:2, and the upscale is a straight stretch — on 16:9 the mesh came out
   * horizontally smeared. Node layout is independent of field resolution, so
   * the field is sized to the actual aspect instead. Same pixel budget, same
   * design, no distortion.
   */
  function ensureField(w: number, h: number) {
    const aspect = w / Math.max(1, h);
    const nextBw = Math.max(16, Math.min(120, Math.round(Math.sqrt(MESH_FIELD_PIXELS * aspect))));
    const nextBh = Math.max(16, Math.min(120, Math.round(MESH_FIELD_PIXELS / nextBw)));
    if (nextBw === bw && nextBh === bh && buf && img) return;

    bw = nextBw;
    bh = nextBh;
    const canvas = document.createElement('canvas');
    canvas.width = bw;
    canvas.height = bh;
    buf = { canvas, ctx: canvas.getContext('2d') as CanvasRenderingContext2D };
    img = buf.ctx.createImageData(bw, bh);
  }

  return {
    draw(ctx, w, h, p) {
      ensureField(w, h);
      if (!buf || !img) return;

      for (let i = 0; i < nodes.length; i++) {
        const n = nodes[i];
        nodeX[i] = n.gx + n.ax * Math.sin(p * n.fx + n.px);
        nodeY[i] = n.gy + n.ay * Math.sin(p * n.fy + n.py);
        nodeHue[i] = (n.hue + 24 * Math.sin(p + n.px) + 360) % 360;
        const rgb = hsv2rgb(nodeHue[i], 0.78, n.light);
        nodeR[i] = rgb[0];
        nodeG[i] = rgb[1];
        nodeB[i] = rgb[2];
      }

      const d = img.data;
      for (let y = 0; y < bh; y++) {
        const v = y / (bh - 1);
        for (let x = 0; x < bw; x++) {
          const u = x / (bw - 1);
          let cr = 0;
          let cg = 0;
          let cb = 0;
          let cw = 0;
          for (let r = 0; r < ROWS; r++) {
            for (let c = 0; c < COLS; c++) {
              const idx = r * COLS + c;
              const wq =
                (1 / (1 + Math.abs(u - nodeX[idx]) * 7.5)) *
                (1 / (1 + Math.abs(v - nodeY[idx]) * 7.5));
              cr += nodeR[idx] * wq;
              cg += nodeG[idx] * wq;
              cb += nodeB[idx] * wq;
              cw += wq;
            }
          }
          const k = (y * bw + x) * 4;
          d[k] = cr / cw;
          d[k + 1] = cg / cw;
          d[k + 2] = cb / cw;
          d[k + 3] = 255;
        }
      }
      buf.ctx.putImageData(img, 0, 0);

      ctx.imageSmoothingEnabled = true;
      ctx.imageSmoothingQuality = 'high';
      ctx.drawImage(buf.canvas, 0, 0, w, h);

      // Slow drifting vignette keeps the corners from matching the centre.
      const vg = ctx.createRadialGradient(
        w * (0.5 + 0.12 * Math.sin(p)),
        h * (0.5 + 0.1 * Math.cos(p * 2)),
        0,
        w * 0.5,
        h * 0.5,
        Math.max(w, h) * 0.85,
      );
      vg.addColorStop(0, 'hsla(0,0%,0%,0)');
      vg.addColorStop(1, 'hsla(230,40%,4%,0.55)');
      ctx.fillStyle = vg;
      ctx.fillRect(0, 0, w, h);
    },
  };
}

function hsv2rgb(h: number, s: number, v: number): [number, number, number] {
  const cc = v * s;
  const hp = h / 60;
  const xx = cc * (1 - Math.abs((hp % 2) - 1));
  const m = v - cc;
  let r: number;
  let g: number;
  let b: number;
  if (hp < 1) {
    r = cc; g = xx; b = 0;
  } else if (hp < 2) {
    r = xx; g = cc; b = 0;
  } else if (hp < 3) {
    r = 0; g = cc; b = xx;
  } else if (hp < 4) {
    r = 0; g = xx; b = cc;
  } else if (hp < 5) {
    r = xx; g = 0; b = cc;
  } else {
    r = cc; g = 0; b = xx;
  }
  return [(r + m) * 255, (g + m) * 255, (b + m) * 255];
}

/* ------------------------------------------------------------------- nebula */

interface Particle {
  hue: number;
  x: number;
  y: number;
  ax: number;
  ay: number;
  fx: number;
  fy: number;
  px: number;
  py: number;
  r: number;
  a: number;
  tw: number;
}

function createNebula(): WallpaperScene {
  const rnd = mulberry32(0x2c0f);
  const HUES = [196, 210, 264, 300, 330, 40, 168];
  const parts: Particle[] = [];
  for (let i = 0; i < 58; i++) {
    parts.push({
      hue: HUES[i % HUES.length],
      x: rnd(),
      y: rnd(),
      ax: 0.05 + rnd() * 0.16,
      ay: 0.04 + rnd() * 0.13,
      fx: 1 + Math.floor(rnd() * 2),
      fy: 1 + Math.floor(rnd() * 3),
      px: rnd() * TAU,
      py: rnd() * TAU,
      r: 0.006 + rnd() * 0.022,
      a: 0.35 + rnd() * 0.5,
      tw: 1 + Math.floor(rnd() * 3),
    });
  }

  return {
    draw(ctx, w, h, p) {
      const g0 = ctx.createRadialGradient(
        w * 0.5, h * 0.5, 0,
        w * 0.5, h * 0.5, Math.max(w, h) * 0.8,
      );
      g0.addColorStop(0, '#0b1024');
      g0.addColorStop(0.6, '#070a16');
      g0.addColorStop(1, '#04060c');
      ctx.fillStyle = g0;
      ctx.fillRect(0, 0, w, h);

      ctx.globalCompositeOperation = 'lighter';
      for (let k = 0; k < 3; k++) {
        const cx = w * (0.3 + 0.4 * Math.sin(p + k));
        const cy = h * (0.3 + 0.4 * Math.cos(p * 2 + k));
        const rr = Math.max(w, h) * (0.42 + 0.1 * k);
        const gg = ctx.createRadialGradient(cx, cy, 0, cx, cy, rr);
        gg.addColorStop(0, `hsla(${250 + k * 40},80%,42%,0.2)`);
        gg.addColorStop(1, 'hsla(0,0%,0%,0)');
        ctx.fillStyle = gg;
        ctx.beginPath();
        ctx.arc(cx, cy, rr, 0, TAU);
        ctx.fill();
      }

      const diag = Math.sqrt(w * w + h * h);
      for (let i = 0; i < parts.length; i++) {
        const q = parts[i];
        const x = w * (q.x + q.ax * Math.sin(p * q.fx + q.px));
        const y = h * (q.y + q.ay * Math.sin(p * q.fy + q.py));
        const tw = 0.82 + 0.18 * Math.sin(p * q.tw + q.px);
        const r = diag * q.r * tw;
        const hue = (q.hue + 16 * Math.sin(p + q.px)) % 360;
          const g = ctx.createRadialGradient(x, y, 0, x, y, r);
          g.addColorStop(0.0, `hsla(${hue},100%,92%,${q.a * tw})`);
          g.addColorStop(0.3, `hsla(${hue},98%,72%,${q.a * 0.5})`);
          g.addColorStop(0.7, `hsla(${(hue + 30) % 360},94%,56%,${q.a * 0.12})`);
        g.addColorStop(1.0, 'hsla(0,0%,0%,0)');
        ctx.fillStyle = g;
        ctx.beginPath();
        ctx.arc(x, y, r, 0, TAU);
        ctx.fill();
      }
      ctx.globalCompositeOperation = 'source-over';
    },
  };
}

/* -------------------------------------------------------------------- index */

const FACTORIES: Record<Exclude<WallpaperId, 'none'>, () => WallpaperScene> = {
  waves: createWaves,
  ribbons: createRibbons,
  mesh: createMesh,
  nebula: createNebula,
};

export function isWallpaperId(value: unknown): value is WallpaperId {
  return typeof value === 'string' && (WALLPAPER_IDS as readonly string[]).includes(value);
}

export function isAnimatedWallpaper(id: WallpaperId): id is Exclude<WallpaperId, 'none'> {
  return id !== 'none';
}

/** One shared instance per id: the scenes hold per-instance buffers that are
 *  fully rewritten each frame, so reuse is safe and avoids rebuilding the mesh
 *  field on every wallpaper change. */
const cache = new Map<Exclude<WallpaperId, 'none'>, WallpaperScene>();

export function getWallpaperScene(id: Exclude<WallpaperId, 'none'>): WallpaperScene {
  let scene = cache.get(id);
  if (!scene) {
    scene = FACTORIES[id]();
    cache.set(id, scene);
  }
  return scene;
}
