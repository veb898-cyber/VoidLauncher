/* ---------------------------------------------------------------------------
 * Liquid Glass background lab — scene library.
 *
 * Seventeen hand-written, fully procedural, seamlessly looping animated
 * backgrounds built specifically to make Liquid Glass effects measurable:
 *   - broad luminance ranges   (does blur read at all?)
 *   - saturated colour fields  (is saturate() doing anything?)
 *   - thin / high-frequency detail (how large does the blur radius look?)
 *   - hard light/dark borders  (is the rim visible on both polarities?)
 *   - small text over both extremes (is foreground contrast still safe?)
 *   - a scrolling content feed (does a full-height chrome panel still hold
 *     11px text over moving rows?)
 *
 * Every time-dependent term is sin/cos of an INTEGER multiple of the loop
 * phase, so a scene returns to its exact starting image after one full
 * period — drawing at phase 0 and phase TAU produces identical pixels.
 * Verified by the loop test in the lab.
 *
 * Plain classic script (no ES modules, no fetch) so index.html opens straight
 * from the filesystem. No external assets, no libraries, no video, no GIF.
 * ------------------------------------------------------------------------- */

(function (global) {
  'use strict';

  var TAU = Math.PI * 2;

  function lerp(a, b, t) { return a + (b - a) * t; }
  function clamp(v, a, b) { return v < a ? a : (v > b ? b : v); }
  function smoothstep(t) { return t * t * (3 - 2 * t); }

  /* --- deterministic rng, so every reload looks identical ---------------- */
  function mulberry32(seed) {
    var a = seed >>> 0;
    return function () {
      a = (a + 0x6d2b79f5) | 0;
      var t = Math.imul(a ^ (a >>> 15), 1 | a);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  /* --- 3D value noise. The third axis is fed a circle (cos/sin of the loop
         phase), so every noise-driven scene returns to its exact start. ---- */
  function hash3(x, y, z) {
    var h = Math.imul(x | 0, 374761393) + Math.imul(y | 0, 668265263) + Math.imul(z | 0, 1442695041);
    h = Math.imul(h ^ (h >>> 13), 1274126177);
    return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
  }

  function noise3(x, y, z) {
    var xi = Math.floor(x), yi = Math.floor(y), zi = Math.floor(z);
    var u = smoothstep(x - xi), v = smoothstep(y - yi), w = smoothstep(z - zi);
    var c000 = hash3(xi, yi, zi),         c100 = hash3(xi + 1, yi, zi);
    var c010 = hash3(xi, yi + 1, zi),     c110 = hash3(xi + 1, yi + 1, zi);
    var c001 = hash3(xi, yi, zi + 1),     c101 = hash3(xi + 1, yi, zi + 1);
    var c011 = hash3(xi, yi + 1, zi + 1), c111 = hash3(xi + 1, yi + 1, zi + 1);
    var x00 = lerp(c000, c100, u), x10 = lerp(c010, c110, u);
    var x01 = lerp(c001, c101, u), x11 = lerp(c011, c111, u);
    return lerp(lerp(x00, x10, v), lerp(x01, x11, v), w);
  }

  function fbm3(x, y, z, octaves, gain, lacunarity) {
    var sum = 0, amp = 0.5, norm = 0, fx = x, fy = y, fz = z;
    for (var i = 0; i < octaves; i++) {
      sum += amp * noise3(fx, fy, fz);
      norm += amp;
      amp *= gain; fx *= lacunarity; fy *= lacunarity; fz *= lacunarity;
    }
    return sum / norm;
  }

  /* --- smooth colour ramp ------------------------------------------------ */
  function ramp(stops, t) {
    t = clamp(t, 0, 1);
    for (var i = 0; i < stops.length - 1; i++) {
      var a = stops[i], b = stops[i + 1];
      if (t >= a[0] && t <= b[0]) {
        var k = smoothstep((t - a[0]) / (b[0] - a[0] || 1));
        return [lerp(a[1], b[1], k), lerp(a[2], b[2], k), lerp(a[3], b[3], k)];
      }
    }
    var last = stops[stops.length - 1];
    return [last[1], last[2], last[3]];
  }

  var PLASMA_RAMP = [
    [0.00, 8, 14, 38], [0.28, 22, 78, 132], [0.52, 96, 52, 168],
    [0.74, 206, 84, 128], [1.00, 252, 206, 132]
  ];
  var SMOKE_RAMP = [
    [0.00, 4, 6, 14], [0.35, 18, 44, 86], [0.62, 96, 44, 128],
    [0.84, 206, 118, 96], [1.00, 246, 226, 198]
  ];
  var MESH_HUES = [206, 268, 322, 38, 168];

  /* --- low-res field buffer, upscaled with smoothing --------------------- */
  function fieldBuffer(w, h) {
    var c = document.createElement('canvas');
    c.width = w; c.height = h;
    return { canvas: c, ctx: c.getContext('2d') };
  }

  function paintField(target, buf, w, h) {
    target.imageSmoothingEnabled = true;
    target.imageSmoothingQuality = 'high';
    target.drawImage(buf.canvas, 0, 0, w, h);
  }

  /* ---------------------------------------------------------------------
   * Atmosphere.
   *
   * A material can only show what there is underneath it. A background that
   * leaves a third of the frame near-black gives the glass nothing to work
   * with, and no amount of blur or saturation tuning fixes that. So every
   * scene starts from this: a slowly shifting low-frequency base plus a few
   * very large, very soft washes.
   *
   * Deliberately built from the lowest possible spatial frequency — broad,
   * slow, no detail — so raising the floor never competes with the glass
   * for attention. All motion is an integer multiple of the loop phase, so
   * the seamlessness guarantee is preserved.
   * ------------------------------------------------------------------- */
  function atmosphere(ctx, w, h, p, o) {
    var diag = Math.sqrt(w * w + h * h);
    var hue = (o.hue || 224) + 14 * Math.sin(p);
    var sat = o.sat == null ? 28 : o.sat;
    var lo = o.lo == null ? 0.07 : o.lo;
    var hi = o.hi == null ? 0.12 : o.hi;

    var g = ctx.createLinearGradient(0, 0, w * 0.3, h);
    g.addColorStop(0.00, 'hsl(' + ((hue + 10) % 360) + ',' + sat + '%,' + (hi * 100).toFixed(1) + '%)');
    g.addColorStop(0.55, 'hsl(' + hue + ',' + sat + '%,' + ((hi + lo) * 50).toFixed(1) + '%)');
    g.addColorStop(1.00, 'hsl(' + ((hue + 18) % 360) + ',' + (sat + 4) + '%,' + (lo * 100).toFixed(1) + '%)');
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, w, h);

    var washes = o.washes || [];
    ctx.globalCompositeOperation = 'lighter';
    for (var i = 0; i < washes.length; i++) {
      var b = washes[i];
      var x = w * (0.5 + b.sx * Math.sin(p * b.fx + b.px));
      var y = h * (0.5 + b.sy * Math.sin(p * b.fy + b.py));
      var r = diag * b.r;
      var wh = (b.hue + 20 * Math.sin(p + b.px) + 360) % 360;
      var rg = ctx.createRadialGradient(x, y, 0, x, y, r);
      rg.addColorStop(0.00, 'hsla(' + wh + ',' + (b.sat || 60) + '%,' + (b.light || 54) + '%,' + b.a + ')');
      rg.addColorStop(0.45, 'hsla(' + wh + ',' + (b.sat || 60) + '%,' + (b.light || 54) + '%,' + (b.a * 0.42) + ')');
      rg.addColorStop(0.78, 'hsla(' + wh + ',' + (b.sat || 60) + '%,' + (b.light || 54) + '%,' + (b.a * 0.10) + ')');
      rg.addColorStop(1.00, 'hsla(0,0%,0%,0)');
      ctx.fillStyle = rg;
      ctx.fillRect(0, 0, w, h);
    }
    ctx.globalCompositeOperation = 'source-over';
  }

  /* =======================================================================
   * 1. Slow Aurora — huge, very soft translucent curtains of colour.
   *    Tests: broad luminance gradient, saturation over large areas,
   *    how weak the blur reads when the source is already soft.
   * ===================================================================== */
  function aurora() {
    var bands = [
      { hue: 152, y: 0.26, r: 0.52, sp: 1, a: 0.40 },
      { hue: 186, y: 0.44, r: 0.46, sp: 2, a: 0.35 },
      { hue: 268, y: 0.34, r: 0.50, sp: 1, a: 0.33 },
      { hue: 318, y: 0.58, r: 0.40, sp: 3, a: 0.26 },
      { hue: 96,  y: 0.68, r: 0.38, sp: 2, a: 0.22 }
    ];
    return {
      draw: function (ctx, w, h, p) {
        /* broad, low-contrast atmosphere so no part of the frame is dead */
        atmosphere(ctx, w, h, p, {
          hue: 226, sat: 30, lo: 0.065, hi: 0.110,
          washes: [
            { hue: 198, a: 0.13, r: 0.92, sx: 0.26, sy: 0.20, fx: 1, fy: 1, px: 0.4, py: 1.1 },
            { hue: 266, a: 0.10, r: 0.88, sx: 0.24, sy: 0.22, fx: 1, fy: 1, px: 2.4, py: 2.2 },
            { hue: 156, a: 0.075, r: 0.74, sx: 0.30, sy: 0.16, fx: 1, fy: 1, px: 4.1, py: 0.3 }
          ]
        });
        ctx.globalCompositeOperation = 'lighter';
        var diag = Math.sqrt(w * w + h * h);
        for (var i = 0; i < bands.length; i++) {
          var b = bands[i];
          var sp = b.sp;
          var x = w * (0.5 + 0.34 * Math.sin(p * sp + i * 1.9));
          var y = h * (b.y + 0.16 * Math.sin(p * (sp + 1) + i * 2.7));
          var r = diag * b.r * (0.9 + 0.12 * Math.sin(p * 2 + i));
          var hue = (b.hue + 22 * Math.sin(p + i)) % 360;
          var g = ctx.createRadialGradient(x, y, 0, x, y, r);
          g.addColorStop(0.00, 'hsla(' + hue + ',92%,66%,' + b.a + ')');
          g.addColorStop(0.35, 'hsla(' + ((hue + 26) % 360) + ',88%,54%,' + (b.a * 0.5) + ')');
          g.addColorStop(0.70, 'hsla(' + ((hue + 52) % 360) + ',84%,46%,' + (b.a * 0.16) + ')');
          g.addColorStop(1.00, 'hsla(0,0%,0%,0)');
          ctx.fillStyle = g;
          ctx.save();
          ctx.translate(x, y);
          ctx.rotate(Math.sin(p * (sp + 2) + i) * 0.5);
          ctx.scale(1, 0.52 + 0.14 * Math.sin(p * 2 + i * 1.4));
          ctx.beginPath();
          ctx.arc(0, 0, r, 0, TAU);
          ctx.fill();
          ctx.restore();
        }
        ctx.globalCompositeOperation = 'source-over';
      }
    };
  }

  /* =======================================================================
   * 2. Fluid Gradient — big gradient spots drifting on independent paths.
   *    Tests: how the material reacts to brightness moving underneath it,
   *    and whether a fixed tint reads as adaptive or as a flat wash.
   * ===================================================================== */
  function fluid() {
    var rnd = mulberry32(0x51A7);
    var blobs = [];
    for (var i = 0; i < 6; i++) {
      blobs.push({
        hue: [204, 268, 322, 28, 168, 46][i],
        ax: 0.16 + rnd() * 0.26, ay: 0.12 + rnd() * 0.22,
        cx: 0.22 + rnd() * 0.56, cy: 0.22 + rnd() * 0.56,
        fx: 1 + Math.floor(rnd() * 2), fy: 1 + Math.floor(rnd() * 3),
        px: rnd() * TAU, py: rnd() * TAU,
        r: 0.30 + rnd() * 0.22, a: 0.33 + rnd() * 0.14
      });
    }
    return {
      draw: function (ctx, w, h, p) {
        /* already the best-covered scene; the base just guarantees the
           corners never fall to black when the blobs drift away */
        atmosphere(ctx, w, h, p, {
          hue: 218, sat: 30, lo: 0.055, hi: 0.095,
          washes: [
            { hue: 210, a: 0.10, r: 0.90, sx: 0.26, sy: 0.20, fx: 1, fy: 1, px: 0.3, py: 1.5 },
            { hue: 292, a: 0.08, r: 0.86, sx: 0.24, sy: 0.22, fx: 1, fy: 1, px: 2.2, py: 0.7 }
          ]
        });
        ctx.globalCompositeOperation = 'lighter';
        var diag = Math.sqrt(w * w + h * h);
        for (var i = 0; i < blobs.length; i++) {
          var b = blobs[i];
          var x = w * clamp(b.cx + b.ax * Math.sin(p * b.fx + b.px), -0.1, 1.1);
          var y = h * clamp(b.cy + b.ay * Math.sin(p * b.fy + b.py), -0.1, 1.1);
          var r = diag * b.r;
          var hue = (b.hue + 26 * Math.sin(p + b.px)) % 360;
          var g = ctx.createRadialGradient(x, y, 0, x, y, r);
          g.addColorStop(0.00, 'hsla(' + hue + ',94%,62%,' + b.a + ')');
          g.addColorStop(0.40, 'hsla(' + ((hue + 18) % 360) + ',90%,52%,' + (b.a * 0.46) + ')');
          g.addColorStop(0.75, 'hsla(' + ((hue + 40) % 360) + ',86%,46%,' + (b.a * 0.12) + ')');
          g.addColorStop(1.00, 'hsla(0,0%,0%,0)');
          ctx.fillStyle = g;
          ctx.beginPath();
          ctx.arc(x, y, r, 0, TAU);
          ctx.fill();
        }
        ctx.globalCompositeOperation = 'source-over';
      }
    };
  }

  /* =======================================================================
   * 3. Chromatic Waves — slow stacked waves, hue rotating as they travel.
   *    Tests: hue shift under saturate(), blur on repeating soft edges,
   *    and rim legibility against alternating light and dark bands.
   * ===================================================================== */
  function waves() {
    return {
      draw: function (ctx, w, h, p) {
        /* bands used to start at 16% and stop at 84% of the height, leaving
           dark bands top and bottom; the base now carries those areas */
        atmosphere(ctx, w, h, p, {
          hue: 220, sat: 28, lo: 0.055, hi: 0.090,
          washes: [
            { hue: 194, a: 0.09, r: 0.90, sx: 0.24, sy: 0.20, fx: 1, fy: 1, px: 0.8, py: 1.4 },
            { hue: 300, a: 0.075, r: 0.86, sx: 0.22, sy: 0.22, fx: 1, fy: 1, px: 2.7, py: 0.2 }
          ]
        });
        ctx.globalCompositeOperation = 'lighter';
        var bands = 8;
        for (var i = 0; i < bands; i++) {
          var t = i / (bands - 1);
          var rate = 1 + Math.round(t * 2);
          var amp = h * (0.05 + 0.10 * Math.sin(t * Math.PI));
          var baseY = h * (0.08 + 0.84 * t);
          var drift = p * rate;
          var hue = (196 + i * 22 + 40 * Math.sin(p + i)) % 360;
          var g = ctx.createLinearGradient(0, baseY - amp * 2, 0, baseY + amp * 3);
          g.addColorStop(0.00, 'hsla(' + hue + ',96%,64%,0)');
          g.addColorStop(0.22, 'hsla(' + hue + ',96%,62%,0.46)');
          g.addColorStop(0.55, 'hsla(' + ((hue + 34) % 360) + ',92%,52%,0.26)');
          g.addColorStop(1.00, 'hsla(' + ((hue + 60) % 360) + ',88%,44%,0)');
          ctx.fillStyle = g;
          ctx.beginPath();
          ctx.moveTo(0, h + 4);
          var steps = 64;
          for (var s = 0; s <= steps; s++) {
            var u = s / steps;
            var y = baseY
              + Math.sin(u * 6.0 + drift) * amp
              + Math.sin(u * 2.3 - drift * 2) * amp * 0.6
              + Math.sin(u * 11.0 + drift * 3) * amp * 0.18;
            ctx.lineTo(u * w, y);
          }
          ctx.lineTo(w, h + 4);
          ctx.closePath();
          ctx.fill();
        }
        ctx.globalCompositeOperation = 'source-over';
      }
    };
  }

  /* =======================================================================
   * 4. Orbital Blobs — a few glowing bodies on different orbits.
   *    Tests: small bright highlights on a dark field, rim visibility,
   *    specular, and how a moving highlight interacts with a fixed tint.
   * ===================================================================== */
  function orbital() {
    var bodies = [
      { hue: 198, rx: 0.30, ry: 0.26, cx: 0.44, cy: 0.50, n: 1, sp: 1, r: 0.115, a: 0.95, ph: 0.0 },
      { hue: 276, rx: 0.40, ry: 0.20, cx: 0.50, cy: 0.46, n: 2, sp: 2, r: 0.085, a: 0.80, ph: 1.9 },
      { hue: 334, rx: 0.22, ry: 0.36, cx: 0.48, cy: 0.52, n: 1, sp: 1, r: 0.070, a: 0.70, ph: 3.4 },
      { hue: 42,  rx: 0.34, ry: 0.30, cx: 0.46, cy: 0.48, n: 3, sp: 3, r: 0.055, a: 0.55, ph: 5.1 },
      { hue: 168, rx: 0.46, ry: 0.16, cx: 0.52, cy: 0.44, n: 2, sp: 1, r: 0.048, a: 0.48, ph: 2.6 }
    ];
    return {
      draw: function (ctx, w, h, p) {
        /* Orbital Blobs stays the darkest scene on purpose — it is the only
           one that tests rim and specular on a near-black field. The base is
           lifted just enough that a body crossing a corner still has a
           gradient to travel across, without turning it into a bright scene. */
        atmosphere(ctx, w, h, p, {
          hue: 230, sat: 26, lo: 0.040, hi: 0.070,
          washes: [
            { hue: 214, a: 0.09, r: 0.90, sx: 0.26, sy: 0.20, fx: 1, fy: 1, px: 1.7, py: 0.9 },
            { hue: 276, a: 0.07, r: 0.86, sx: 0.24, sy: 0.22, fx: 1, fy: 1, px: 3.6, py: 2.6 }
          ]
        });
        ctx.globalCompositeOperation = 'lighter';
        for (var i = 0; i < bodies.length; i++) {
          var b = bodies[i];
          var ang = p * b.sp + b.ph;
          var x = w * (b.cx + b.rx * Math.cos(ang));
          var y = h * (b.cy + b.ry * Math.sin(ang * b.n));
          var r = Math.sqrt(w * w + h * h) * b.r * (0.92 + 0.12 * Math.sin(p * 2 + b.ph));
          var hue = (b.hue + 18 * Math.sin(p + b.ph)) % 360;
          var g = ctx.createRadialGradient(x, y, 0, x, y, r);
          g.addColorStop(0.00, 'hsla(' + hue + ',100%,86%,' + b.a + ')');
          g.addColorStop(0.18, 'hsla(' + hue + ',98%,68%,' + (b.a * 0.72) + ')');
          g.addColorStop(0.48, 'hsla(' + ((hue + 26) % 360) + ',94%,54%,' + (b.a * 0.28) + ')');
          g.addColorStop(0.78, 'hsla(' + ((hue + 48) % 360) + ',90%,46%,' + (b.a * 0.07) + ')');
          g.addColorStop(1.00, 'hsla(0,0%,0%,0)');
          ctx.fillStyle = g;
          ctx.beginPath();
          ctx.arc(x, y, r, 0, TAU);
          ctx.fill();
        }
        ctx.globalCompositeOperation = 'source-over';
      }
    };
  }

  /* =======================================================================
   * 5. Liquid Ribbons — flowing ribbons with slow deformation.
   *    Tests: thin curved structure, edge definition, how the blur radius
   *    reads against narrow shapes, and rim along a long soft border.
   * ===================================================================== */
  function ribbons() {
    var N = 110;
    return {
      draw: function (ctx, w, h, p) {
        /* the floor matters most here: ribbons are thin and the gaps between
           them used to be dead black, so the glass had nothing to distort */
        atmosphere(ctx, w, h, p, {
          hue: 232, sat: 30, lo: 0.055, hi: 0.090,
          washes: [
            { hue: 206, a: 0.10, r: 0.90, sx: 0.24, sy: 0.18, fx: 1, fy: 1, px: 1.2, py: 0.6 },
            { hue: 284, a: 0.075, r: 0.84, sx: 0.22, sy: 0.24, fx: 1, fy: 1, px: 3.1, py: 1.9 }
          ]
        });
        var count = 5;
        for (var k = 0; k < count; k++) {
          var rate = k + 1;
          var baseY = h * (0.18 + 0.16 * k);
          var hue = (192 + k * 54 + 28 * Math.sin(p + k)) % 360;
          var half = h * (0.072 + 0.030 * Math.sin(p * 2 + k));

          /* a wide, soft glow under each ribbon, so a ribbon reads as a
             luminous band rather than an isolated stroke on black */
          ctx.globalCompositeOperation = 'lighter';
          var gw = ctx.createLinearGradient(0, baseY - half * 5, 0, baseY + half * 5);
          gw.addColorStop(0.00, 'hsla(' + hue + ',90%,60%,0)');
          gw.addColorStop(0.50, 'hsla(' + hue + ',90%,58%,0.10)');
          gw.addColorStop(1.00, 'hsla(' + hue + ',90%,60%,0)');
          ctx.fillStyle = gw;
          ctx.fillRect(0, baseY - half * 5, w, half * 10);

          var top = [], bot = [];
          for (var i = 0; i <= N; i++) {
            var u = i / N;
            var wob = Math.sin(u * 5.1 + p * rate + k) * h * 0.10
                    + Math.sin(u * 10.7 - p * 2 + k) * h * 0.042
                    + Math.sin(u * 2.2 + p) * h * 0.065;
            var th = half * (0.5 + 0.5 * Math.sin(u * 6.6 - p * 2 + k));
            top.push([u * w, baseY + wob - th]);
            bot.push([u * w, baseY + wob + th]);
          }
          var g = ctx.createLinearGradient(0, 0, w, 0);
          g.addColorStop(0.00, 'hsla(' + hue + ',94%,66%,0)');
          g.addColorStop(0.22, 'hsla(' + hue + ',94%,64%,0.54)');
          g.addColorStop(0.50, 'hsla(' + ((hue + 30) % 360) + ',92%,58%,0.42)');
          g.addColorStop(0.80, 'hsla(' + ((hue + 52) % 360) + ',90%,60%,0.47)');
          g.addColorStop(1.00, 'hsla(' + ((hue + 52) % 360) + ',90%,60%,0)');
          ctx.globalCompositeOperation = 'source-over';
          ctx.fillStyle = g;
          ctx.beginPath();
          ctx.moveTo(top[0][0], top[0][1]);
          for (var a = 1; a <= N; a++) ctx.lineTo(top[a][0], top[a][1]);
          for (var b2 = N; b2 >= 0; b2--) ctx.lineTo(bot[b2][0], bot[b2][1]);
          ctx.closePath();
          ctx.fill();

          /* Thin specular core, so narrow highlights are available to test.
             Kept deliberately dim: a near-white line blooms under blur and
             was the hottest pixel in the lab, which both overloads the
             material and drops body text under it below AA. */
          ctx.globalCompositeOperation = 'lighter';
          ctx.strokeStyle = 'hsla(' + ((hue + 20) % 360) + ',100%,82%,0.16)';
          ctx.lineWidth = Math.max(1, h * 0.005);
          ctx.beginPath();
          for (var c = 0; c <= N; c++) {
            var mx = (top[c][0] + bot[c][0]) * 0.5;
            var my = (top[c][1] + bot[c][1]) * 0.5;
            if (c === 0) ctx.moveTo(mx, my); else ctx.lineTo(mx, my);
          }
          ctx.stroke();
          ctx.globalCompositeOperation = 'source-over';
        }
      }
    };
  }

  /* =======================================================================
   * 6. Soft Plasma — slow procedural field, mid-frequency detail.
   *    Tests: the classic "everything moves" case and the best comparison
   *    bed for reading blur radius differences side by side.
   * ===================================================================== */
  function plasma() {
    var BW = 72, BH = 46;
    var buf = fieldBuffer(BW, BH);
    var img = buf.ctx.createImageData(BW, BH);
    return {
      draw: function (ctx, w, h, p) {
        var cz = Math.cos(p), sz = Math.sin(p);
        var d = img.data;
        for (var y = 0; y < BH; y++) {
          for (var x = 0; x < BW; x++) {
            var u = x / BW * 4.2, v = y / BH * 3.0;
            var n = fbm3(u + cz * 0.6, v + sz * 0.6, cz * 0.5 + 2.0, 4, 0.5, 2.0);
            var s = fbm3(u * 1.7 - sz * 0.4, v * 1.7 + cz * 0.4, sz * 0.5 + 7.0, 3, 0.5, 2.0);
            var t = clamp(n * 0.68 + s * 0.42 - 0.05, 0, 1);
            var c = ramp(PLASMA_RAMP, t);
            var i = (y * BW + x) * 4;
            d[i] = c[0]; d[i + 1] = c[1]; d[i + 2] = c[2]; d[i + 3] = 255;
          }
        }
        buf.ctx.putImageData(img, 0, 0);
        paintField(ctx, buf, w, h);
      }
    };
  }

  /* =======================================================================
   * 7. Particle Nebula — a small number of soft glowing motes.
   *    Tests: fine detail, saturate() on small bright points, blur on point
   *    highlights, and rim where a mote passes under a material edge.
   * ===================================================================== */
  function nebula() {
    var rnd = mulberry32(0x2C0F);
    var parts = [];
    for (var i = 0; i < 58; i++) {
      parts.push({
        hue: [196, 210, 264, 300, 330, 40, 168][i % 7],
        x: rnd(), y: rnd(),
        ax: 0.05 + rnd() * 0.16, ay: 0.04 + rnd() * 0.13,
        fx: 1 + Math.floor(rnd() * 2), fy: 1 + Math.floor(rnd() * 3),
        px: rnd() * TAU, py: rnd() * TAU,
        r: 0.006 + rnd() * 0.022,
        a: 0.35 + rnd() * 0.5,
        tw: 1 + Math.floor(rnd() * 3)
      });
    }
    return {
      draw: function (ctx, w, h, p) {
        var g0 = ctx.createRadialGradient(w * 0.5, h * 0.5, 0, w * 0.5, h * 0.5, Math.max(w, h) * 0.8);
        g0.addColorStop(0, '#0b1024');
        g0.addColorStop(0.6, '#070a16');
        g0.addColorStop(1, '#04060c');
        ctx.fillStyle = g0;
        ctx.fillRect(0, 0, w, h);
        ctx.globalCompositeOperation = 'lighter';
        for (var k = 0; k < 3; k++) {
          var cx = w * (0.3 + 0.4 * Math.sin(p + k));
          var cy = h * (0.3 + 0.4 * Math.cos(p * 2 + k));
          var rr = Math.max(w, h) * (0.42 + 0.1 * k);
          var gg = ctx.createRadialGradient(cx, cy, 0, cx, cy, rr);
          gg.addColorStop(0, 'hsla(' + (250 + k * 40) + ',80%,42%,0.20)');
          gg.addColorStop(1, 'hsla(0,0%,0%,0)');
          ctx.fillStyle = gg;
          ctx.beginPath(); ctx.arc(cx, cy, rr, 0, TAU); ctx.fill();
        }
        var diag = Math.sqrt(w * w + h * h);
        for (var i = 0; i < parts.length; i++) {
          var q = parts[i];
          var x = w * (q.x + q.ax * Math.sin(p * q.fx + q.px));
          var y = h * (q.y + q.ay * Math.sin(p * q.fy + q.py));
          var tw = 0.82 + 0.18 * Math.sin(p * q.tw + q.px);
          var r = diag * q.r * tw;
          var hue = (q.hue + 16 * Math.sin(p + q.px)) % 360;
          var g = ctx.createRadialGradient(x, y, 0, x, y, r);
          g.addColorStop(0.00, 'hsla(' + hue + ',100%,92%,' + (q.a * tw) + ')');
          g.addColorStop(0.30, 'hsla(' + hue + ',98%,72%,' + (q.a * 0.5) + ')');
          g.addColorStop(0.70, 'hsla(' + ((hue + 30) % 360) + ',94%,56%,' + (q.a * 0.12) + ')');
          g.addColorStop(1.00, 'hsla(0,0%,0%,0)');
          ctx.fillStyle = g;
          ctx.beginPath(); ctx.arc(x, y, r, 0, TAU); ctx.fill();
        }
        ctx.globalCompositeOperation = 'source-over';
      }
    };
  }

  /* =======================================================================
   * 8. Gradient Mesh — large colour regions, each moving independently.
   *    Tests: a fixed dark tint over a slowly changing bright field — the
   *    case that decides whether the material reads as adaptive or inert.
   * ===================================================================== */
  function mesh() {
    var COLS = 4, ROWS = 3;
    var rnd = mulberry32(0x9E31);
    var nodes = [];
    for (var r = 0; r < ROWS; r++) {
      for (var c = 0; c < COLS; c++) {
        nodes.push({
          gx: c / (COLS - 1), gy: r / (ROWS - 1),
          hue: MESH_HUES[(r * COLS + c) % MESH_HUES.length],
          ax: 0.08 + rnd() * 0.10, ay: 0.07 + rnd() * 0.10,
          fx: 1 + Math.floor(rnd() * 2), fy: 1 + Math.floor(rnd() * 2),
          px: rnd() * TAU, py: rnd() * TAU,
          light: 0.42 + rnd() * 0.24
        });
      }
    }
    var BW = 60, BH = 40;
    var buf = fieldBuffer(BW, BH);
    var img = buf.ctx.createImageData(BW, BH);
    var pos = [];
    function nodeAt(i, p) {
      var n = nodes[i];
      return [
        n.gx + n.ax * Math.sin(p * n.fx + n.px),
        n.gy + n.ay * Math.sin(p * n.fy + n.py),
        (n.hue + 24 * Math.sin(p + n.px) + 360) % 360
      ];
    }
    function hsv2rgb(hh, s, v) {
      var cc = v * s, hp = hh / 60, xx = cc * (1 - Math.abs((hp % 2) - 1)), m = v - cc, r, g, b;
      if (hp < 1) { r = cc; g = xx; b = 0; }
      else if (hp < 2) { r = xx; g = cc; b = 0; }
      else if (hp < 3) { r = 0; g = cc; b = xx; }
      else if (hp < 4) { r = 0; g = xx; b = cc; }
      else if (hp < 5) { r = xx; g = 0; b = cc; }
      else { r = cc; g = 0; b = xx; }
      return [(r + m) * 255, (g + m) * 255, (b + m) * 255];
    }
    return {
      draw: function (ctx, w, h, p) {
        for (var i = 0; i < nodes.length; i++) pos[i] = nodeAt(i, p);
        var d = img.data;
        for (var y = 0; y < BH; y++) {
          for (var x = 0; x < BW; x++) {
            var u = x / (BW - 1), v = y / (BH - 1);
            var cr = 0, cg = 0, cb = 0, cw = 0;
            for (var r = 0; r < ROWS; r++) {
              for (var c = 0; c < COLS; c++) {
                var idx = r * COLS + c;
                var wq = (1 / (1 + Math.abs(u - pos[idx][0]) * 7.5)) *
                        (1 / (1 + Math.abs(v - pos[idx][1]) * 7.5));
                var rgb = hsv2rgb(pos[idx][2], 0.78, nodes[idx].light);
                cr += rgb[0] * wq; cg += rgb[1] * wq; cb += rgb[2] * wq; cw += wq;
              }
            }
            var k = (y * BW + x) * 4;
            d[k] = cr / cw; d[k + 1] = cg / cw; d[k + 2] = cb / cw; d[k + 3] = 255;
          }
        }
        buf.ctx.putImageData(img, 0, 0);
        paintField(ctx, buf, w, h);
        /* slow drifting vignette keeps the corners from matching the centre */
        var vg = ctx.createRadialGradient(
          w * (0.5 + 0.12 * Math.sin(p)), h * (0.5 + 0.1 * Math.cos(p * 2)),
          0, w * 0.5, h * 0.5, Math.max(w, h) * 0.85);
        vg.addColorStop(0, 'hsla(0,0%,0%,0)');
        vg.addColorStop(1, 'hsla(230,40%,4%,0.55)');
        ctx.fillStyle = vg;
        ctx.fillRect(0, 0, w, h);
      }
    };
  }

  /* =======================================================================
   * 9. Chromatic Smoke — very slow ink-in-water masses, domain-warped fbm.
   *    Tests: organic soft boundaries, blur over fractal detail, and rim
   *    where a material edge crosses a slowly evaporating plume.
   * ===================================================================== */
  function smoke() {
    var BW = 68, BH = 44;
    var buf = fieldBuffer(BW, BH);
    var img = buf.ctx.createImageData(BW, BH);
    return {
      draw: function (ctx, w, h, p) {
        var cz = Math.cos(p), sz = Math.sin(p);
        var d = img.data;
        for (var y = 0; y < BH; y++) {
          for (var x = 0; x < BW; x++) {
            var u = x / BW * 3.1, v = y / BH * 2.2;
            /* domain warp: the ingredient that makes fbm read as liquid */
            var wx = fbm3(u * 0.7 + cz * 0.35, v * 0.7 + sz * 0.35, cz * 0.4 + 1.0, 3, 0.5, 2.0) - 0.5;
            var wy = fbm3(u * 0.7 - sz * 0.35, v * 0.7 + cz * 0.35, sz * 0.4 + 5.0, 3, 0.5, 2.0) - 0.5;
            var n = fbm3(u * 1.5 + wx * 3.2, v * 1.5 + wy * 3.2, cz * 0.6 + 3.0, 5, 0.52, 2.05);
            n = clamp((n - 0.30) * 2.05, 0, 1);
            n = n * n * (3 - 2 * n);
            var c = ramp(SMOKE_RAMP, n);
            var i = (y * BW + x) * 4;
            d[i] = c[0]; d[i + 1] = c[1]; d[i + 2] = c[2];
            d[i + 3] = 255 * clamp(n * 1.5, 0, 1);
          }
        }
        buf.ctx.putImageData(img, 0, 0);
        ctx.fillStyle = '#04060c';
        ctx.fillRect(0, 0, w, h);
        paintField(ctx, buf, w, h);
      }
    };
  }

  /* =======================================================================
   * 10. Glass Stress Test — deliberately hostile, not pretty.
   *     Everything a material can hide behind is put on screen at once:
   *     hard light/dark borders, fully saturated primaries, 1px rules,
   *     a fine checker, small text, and a moving hotspot. Deliberately
   *     slow so it is still comfortable to stare at.
   * ===================================================================== */
  function stress() {
    return {
      draw: function (ctx, w, h, p) {
        /* 1. hard light / dark split, slowly sliding */
        var split = w * (0.42 + 0.14 * Math.sin(p));
        ctx.fillStyle = '#000000';
        ctx.fillRect(0, 0, w, h);
        ctx.fillStyle = '#ffffff';
        ctx.fillRect(split, 0, w - split, h);

        /* 2. fully saturated primaries along the split */
        var bars = ['#ff0000', '#00ff00', '#0000ff', '#ffff00', '#00ffff', '#ff00ff'];
        var bw = w * 0.022;
        for (var i = 0; i < bars.length; i++) {
          ctx.fillStyle = bars[i];
          ctx.fillRect(split - bw * 3 + i * bw, 0, bw, h);
        }

        /* 3. fine checkerboard, bottom-left */
        var cs = Math.max(4, Math.round(Math.min(w, h) * 0.022));
        ctx.save();
        ctx.beginPath();
        ctx.rect(0, h - cs * 6, cs * 12, cs * 6);
        ctx.clip();
        for (var y = 0; y < 6; y++) {
          for (var x = 0; x < 12; x++) {
            ctx.fillStyle = (x + y) % 2 ? '#ffffff' : '#101010';
            ctx.fillRect(x * cs, h - cs * 6 + y * cs, cs, cs);
          }
        }
        ctx.restore();

        /* 4. 1px rules of graded luminance */
        for (var k = 0; k < 16; k++) {
          var v = Math.round((k / 15) * 255);
          ctx.fillStyle = 'rgb(' + v + ',' + v + ',' + v + ')';
          ctx.fillRect(w * 0.06, h * (0.10 + k * 0.021), w * 0.34, 1);
        }

        /* 5. hard-edged geometry — no soft falloff anywhere near it */
        ctx.fillStyle = '#00ff88';
        ctx.fillRect(w * 0.62, h * 0.16, w * 0.13, h * 0.13);
        ctx.fillStyle = '#ff0066';
        ctx.fillRect(w * 0.78, h * 0.16, w * 0.13, h * 0.13);
        ctx.strokeStyle = '#ffffff';
        ctx.lineWidth = 2;
        ctx.strokeRect(w * 0.62, h * 0.40, w * 0.29, h * 0.22);

        /* 6. small text on both polarities */
        ctx.font = '600 9px ui-monospace, Consolas, monospace';
        ctx.fillStyle = '#000000';
        ctx.fillText('tiny 9px on white 000', w * 0.62, h * 0.70);
        ctx.fillStyle = '#ffffff';
        ctx.fillText('tiny 9px on black 000', w * 0.62, h * 0.755);
        ctx.font = '600 13px ui-monospace, Consolas, monospace';
        ctx.fillStyle = '#000000';
        ctx.fillText('13px regular weight', w * 0.62, h * 0.815);
        ctx.fillStyle = '#ffffff';
        ctx.fillText('13px regular weight', w * 0.62, h * 0.87);

        /* 7. slow moving hotspot, the only soft thing on screen */
        var hx = w * (0.5 + 0.34 * Math.cos(p));
        var hy = h * (0.5 + 0.28 * Math.sin(p * 2));
        ctx.globalCompositeOperation = 'lighter';
        var g = ctx.createRadialGradient(hx, hy, 0, hx, hy, Math.max(w, h) * 0.30);
        g.addColorStop(0.00, 'rgba(255,255,255,0.55)');
        g.addColorStop(0.35, 'rgba(255,240,180,0.18)');
        g.addColorStop(1.00, 'rgba(255,255,255,0)');
        ctx.fillStyle = g;
        ctx.beginPath(); ctx.arc(hx, hy, Math.max(w, h) * 0.30, 0, TAU); ctx.fill();
        ctx.globalCompositeOperation = 'source-over';

        /* 8. saturation bars — reference for what saturate() is amplifying */
        var sat = ['hsl(0,100%,50%)', 'hsl(60,100%,50%)', 'hsl(120,100%,50%)',
                   'hsl(180,100%,50%)', 'hsl(240,100%,50%)', 'hsl(300,100%,50%)'];
        var sw = w / sat.length;
        for (var s = 0; s < sat.length; s++) {
          ctx.fillStyle = sat[s];
          ctx.fillRect(s * sw, 0, sw, h * 0.035);
        }
      }
    };
  }

  /* -------------------------------------------------------------------- */

  /* ------------------------------------------------ diagnostic scenes ---
   * These five are measuring instruments, not decoration. Each one removes
   * every variable except the one it exists to test, so the answer comes from
   * looking rather than from arguing about a number.
   */

  /* A measuring stick, not a picture: a pure greyscale ramp with no chroma at
     all, so saturate() has nothing to work with. Whatever the material does
     here, it does with the recipe alone. */
  function rampScene() {
    return {
      draw: function (ctx, w, h, p) {
        var g = ctx.createLinearGradient(0, 0, w, 0);
        for (var i = 0; i <= 10; i++) {
          var t = i / 10, L = Math.round(255 * t);
          g.addColorStop(t, 'rgb(' + L + ',' + L + ',' + L + ')');
        }
        ctx.fillStyle = g;
        ctx.fillRect(0, 0, w, h);
        /* a slow crosshair keeps the frame moving and the sweep seamless; it
           marks which part of the ramp currently sits under the glass */
        var x = Math.round(w * (0.5 - 0.5 * Math.cos(p)));
        ctx.fillStyle = 'rgb(0,0,0)';
        ctx.fillRect(x - 1, 0, 2, h);
        ctx.fillStyle = 'rgb(255,255,255)';
        ctx.fillRect(x, 0, 1, h);
      }
    };
  }

  /* Mid-tone and moderately saturated on purpose: enough colour to judge a
     tint against, not so much that it flatters the material.
     The gradient runs vertically on purpose. All five boxes stand in a row,
     so a vertical gradient gives every one of them a byte-identical
     backdrop and the row measures the tint level instead of the scenery.
     A diagonal or drifting blob here would quietly ruin the instrument. */
  function tintScene() {
    return {
      draw: function (ctx, w, h, p) {
        var sh = 20 * Math.sin(p);
        var g = ctx.createLinearGradient(0, 0, 0, h);
        g.addColorStop(0.00, 'hsl(' + ((206 + sh + 360) % 360) + ',46%,46%)');
        g.addColorStop(0.50, 'hsl(' + ((262 + sh + 360) % 360) + ',44%,50%)');
        g.addColorStop(1.00, 'hsl(' + ((30 + sh + 360) % 360) + ',48%,44%)');
        ctx.fillStyle = g;
        ctx.fillRect(0, 0, w, h);
      }
    };
  }

  /* A hard dark-to-bright split with a fast seam, so one edge can be judged
     against a light side and a dark side at the same time. */
  function rimScene() {
    return {
      draw: function (ctx, w, h, p) {
        var mid = w * (0.5 + 0.16 * Math.sin(p));
        var t0 = Math.max(0.01, mid / w - 0.05);
        var t1 = Math.min(0.99, mid / w + 0.05);
        var g = ctx.createLinearGradient(0, 0, w, 0);
        g.addColorStop(0, '#0c0f16');
        g.addColorStop(t0, '#10131b');
        g.addColorStop(t1, '#e6eaf2');
        g.addColorStop(1, '#f4f7fb');
        ctx.fillStyle = g;
        ctx.fillRect(0, 0, w, h);
        /* a 1px specular catcher right on the seam */
        ctx.fillStyle = 'rgb(255,255,255)';
        ctx.fillRect(Math.round(mid) - 1, 0, 2, h);
      }
    };
  }

  /* One unit of a content feed: a dim image block, three lines of text, an
     accent chip. The layout is fixed — only the scroll offset moves — so the
     whole card is a rigid translation and loops exactly. */
  function chromeFeedUnit(ctx, x, y, w, unit, i) {
    /* a dim image block: the large soft luminance area that decides whether a
       dense panel reads as glass or as a flat grey box */
    var imgH = unit * 0.44;
    var hue = (206 + i * 53) % 360;
    var g = ctx.createLinearGradient(x, y, x + w, y + imgH);
    g.addColorStop(0, 'hsl(' + hue + ',32%,24%)');
    g.addColorStop(1, 'hsl(' + ((hue + 46) % 360) + ',28%,12%)');
    ctx.fillStyle = g;
    ctx.fillRect(x, y, w, imgH);

    /* text bars only a few pixels tall. A 20px blur has something small to
       destroy here, and that destruction is part of what is being measured:
       if the bars survive a dense panel, the panel is not hiding content. */
    var barH = Math.max(2, unit * 0.016);
    var widths = [0.84, 0.61, 0.43];
    var barY = y + imgH + unit * 0.075;
    var b;
    for (b = 0; b < widths.length; b++) {
      ctx.fillStyle = 'rgba(226,236,252,' + (0.34 - b * 0.08).toFixed(2) + ')';
      ctx.fillRect(x, barY + b * unit * 0.062, w * widths[b], barH);
    }

    /* one saturated chip per unit: accent colour has to survive the panel too */
    ctx.fillStyle = 'hsl(' + ((hue + 180) % 360) + ',64%,58%)';
    ctx.fillRect(x, barY + widths.length * unit * 0.062 + unit * 0.03,
                 unit * 0.16, Math.max(3, unit * 0.05));
  }

  /* The substrate the chrome actually sits on.
   *
   * A titlebar and a sidebar are never judged against an abstract gradient —
   * they are judged against content: rows of small text, blocks of imagery and
   * a bit of colour moving behind them. So this is a content feed rather than a
   * backdrop. If a dense panel still reads as glass and still holds 11px text
   * over this, it will read over the real launcher too.
   *
   * Each unit of the feed is drawn from the same index every frame and the
   * offset advances by exactly one full repeat per period, so the image at
   * phase 0 and at phase TAU is identical. The loop check in the lab verifies
   * that rather than trusting it. */
  function chromeScene() {
    return {
      draw: function (ctx, w, h, p) {
        ctx.fillStyle = '#0a0c12';
        ctx.fillRect(0, 0, w, h);

        var unit = Math.max(90, h * 0.44);
        var span = 3 * unit;
        var off = (p / TAU) * span;
        var pad = w * 0.05;
        var colW = w - pad * 2;
        var i, m, y;

        /* slot i is always the same content, at y = i*unit - off, plus copies
           shifted by whole repeats to cover the viewport. Deriving the content
           from the slot index rather than from the draw order is what keeps
           the scroll seamless. */
        for (i = 0; i < 3; i++) {
          for (m = -1; m <= 1; m++) {
            y = i * unit - off + m * span;
            if (y > h || y + unit < 0) continue;
            chromeFeedUnit(ctx, pad, y, colW, unit, i);
          }
        }

        /* one broad highlight drifting across: large-scale luminance movement is
           what makes a dense panel look flat and dead, so it has to move too */
        var hx = w * (0.5 + 0.44 * Math.sin(p));
        var rg = ctx.createRadialGradient(hx, h * 0.5, 0, hx, h * 0.5, Math.max(w, h) * 0.6);
        rg.addColorStop(0, 'rgba(146,178,255,0.15)');
        rg.addColorStop(1, 'rgba(146,178,255,0)');
        ctx.fillStyle = rg;
        ctx.fillRect(0, 0, w, h);
      }
    };
  }

  /* A razor-sharp black/white edge plus pure primaries. A real lens shows
     chromatic fringing and displaces the edge; a uniform blur can do neither.
     Fringe or displacement here would be proof of actual lensing. */
  function fringeScene() {
    return {
      draw: function (ctx, w, h, p) {
        /* rounded to whole pixels: a fractional edge leaves a partially
           covered column, which softens the very step this card exists to
           put under the glass */
        var mid = Math.round(w * (0.5 + 0.18 * Math.sin(p)));
        ctx.fillStyle = '#000';
        ctx.fillRect(0, 0, w, h);
        ctx.fillStyle = '#fff';
        ctx.fillRect(mid, 0, w - mid, h);
        /* the classic aberration target: fully saturated primaries */
        var bw = w * 0.05, bh = h * 0.11, y = h * 0.70;
        var cols = ['#ff0000', '#00ff00', '#0000ff', '#ffff00', '#00ffff', '#ff00ff'];
        for (var i = 0; i < cols.length; i++) {
          ctx.fillStyle = cols[i];
          ctx.fillRect(w * 0.06 + i * (bw + w * 0.012), y, bw, bh);
        }
        /* 1px lines: the highest spatial frequency available to the material */
        ctx.fillStyle = 'rgb(255,255,255)';
        for (var k = 0; k < 24; k++) ctx.fillRect(0, h * 0.08 + k * 4, w, 1);
      }
    };
  }

  /* A dot grid, text-like bars and one bright point: exactly the content a
     lens visibly bends and concentrates, and exactly what a uniform blur
     flattens into mush. */
  function lensScene() {
    return {
      draw: function (ctx, w, h, p) {
        var sh = 30 * Math.sin(p);
        var g = ctx.createLinearGradient(0, 0, w, h);
        g.addColorStop(0, 'hsl(' + ((210 + sh + 360) % 360) + ',58%,44%)');
        g.addColorStop(1, 'hsl(' + ((280 + sh + 360) % 360) + ',56%,26%)');
        ctx.fillStyle = g;
        ctx.fillRect(0, 0, w, h);

        var drift = 14 * Math.sin(p);

        /* Feature size is the whole point of this card, and the first version
           got it wrong. A 1.6px dot under a 14px blur has ~2.5% of the cell
           area, so it spreads to a couple of percent of its own contrast and
           simply evaporates — on both panels, for different reasons. Anything
           meant to be compared after a blur has to be larger than the blur.
           Coarse discs and thick bars survive; the hairline grid is kept at
           low alpha on purpose, as the control that shows what blur destroys
           and displacement preserves. */
        var step = Math.max(40, Math.round(w / 9));
        var r = Math.round(step * 0.18);
        ctx.fillStyle = 'rgba(255,255,255,0.95)';
        for (var y = step; y < h; y += step) {
          for (var x = step; x < w; x += step) {
            ctx.beginPath();
            ctx.arc(Math.round(x + drift), Math.round(y + drift * 0.5), r, 0, TAU);
            ctx.fill();
          }
        }

        /* the control: a hairline grid, deliberately below the blur radius */
        var fine = Math.max(14, Math.round(w / 26));
        ctx.fillStyle = 'rgba(255,255,255,0.4)';
        for (var fy = Math.round(fine / 2); fy < h; fy += fine) {
          for (var fx = Math.round(fine / 2); fx < w; fx += fine) {
            ctx.fillRect(fx, fy, 2, 2);
          }
        }

        /* Bars thick enough to keep an edge after blurring. A long straight
           edge is what actually reveals bending: blur rounds the corner off,
           displacement drags the whole edge sideways. */
        var barH = Math.max(9, Math.round(h * 0.03));
        ctx.fillStyle = 'rgba(255,255,255,0.8)';
        for (var b = 0; b < 5; b++) {
          var by = Math.round(h * (0.1 + b * 0.075));
          var bw = Math.round(w * (0.24 + 0.42 * Math.abs(Math.sin(b * 1.7 + p))));
          ctx.fillRect(Math.round(w * 0.05), by, bw, barH);
        }

        /* an outlined frame: four long straight edges, the cleanest read on
           whether the corner survives as a corner */
        var pad = Math.round(w * 0.08);
        ctx.strokeStyle = 'rgba(255,255,255,0.7)';
        ctx.lineWidth = Math.max(3, Math.round(w * 0.008));
        ctx.strokeRect(pad, pad, w - pad * 2, h - pad * 2);

        /* one bright point, to show a lens gathering light rather than
           scattering it */
        var dx = w * (0.5 + 0.22 * Math.sin(p));
        var dy = h * (0.5 + 0.18 * Math.cos(p));
        var rr = w * 0.09;
        var rg = ctx.createRadialGradient(dx, dy, 0, dx, dy, rr);
        rg.addColorStop(0, 'rgba(255,255,255,0.95)');
        rg.addColorStop(1, 'rgba(255,255,255,0)');
        ctx.fillStyle = rg;
        ctx.fillRect(dx - rr, dy - rr, rr * 2, rr * 2);
      }
    };
  }

  var SCENES = [
    {
      id: 'aurora', name: 'Slow Aurora', period: 52,
      tagline: 'Крупные полупрозрачные цветовые облака',
      tech: 'Canvas 2D, 5 радиальных градиентов + lighter',
      why: 'Самый низкочастотный фон: почти нет мелких деталей. Показывает, какой ' +
           'радиус blur вообще читается на мягком источнике, и не маскирует ли tint ' +
           'просто плоский оттенок поверх пустого тёмного поля.',
      tests: ['Плавная яркость', 'Насыщенность на больших площадях', 'Слабый blur на мягком источнике'],
      make: aurora
    },
    {
      id: 'fluid', name: 'Fluid Gradient', period: 44,
      tagline: 'Независимо движущиеся градиентные пятна',
      tech: 'Canvas 2D, 6 радиальных градиентов + lighter',
      why: 'Яркость перемещается под стеклом с разной скоростью в разных местах. ' +
           'Показывает, адаптируется ли материал к фону или красит его в свой цвет, ' +
           'и не «плывёт» ли текст, когда под ним меняется контраст.',
      tests: ['Яркость движется под материалом', 'Фиксированный tint vs адаптивный цвет', 'Локальный контраст'],
      make: fluid
    },
    {
      id: 'waves', name: 'Chromatic Waves', period: 40,
      tagline: 'Медленные цветные волны с движением и сменой оттенка',
      tech: 'Canvas 2D, 7 линейных градиентов, ломаные по 64 точки',
      why: 'Чередующиеся светлые и тёмные полосы — прямой тест rim: кромка должна ' +
           'читаться то на тёмной, то на светлой стороне. Цветовой сдвиг показывает, ' +
           'что saturate() не пересвечивает материал в неестественный оттенок.',
      tests: ['Сдвиг оттенка под saturate()', 'Blur на повторяющихся кромках', 'Rim на чередовании света и тени'],
      make: waves
    },
    {
      id: 'orbital', name: 'Orbital Blobs', period: 46,
      tagline: 'Светящиеся объекты на разных траекториях',
      tech: 'Canvas 2D, 5 тел, радиальные градиенты + lighter',
      why: 'Мало света и много тёмного, но есть яркие движущиеся блики. Проверяет ' +
           'specular и rim в почти тёмной сцене: если кромка и блик пропадают на чёрном, ' +
           'материал не работает на тёмных темах.',
      tests: ['Яркие акценты на тёмном фоне', 'Видимость rim', 'Блик и specular'],
      make: orbital
    },
    {
      id: 'ribbons', name: 'Liquid Ribbons', period: 38,
      tagline: 'Плавные ленты с медленной деформацией',
      tech: 'Canvas 2D, 4 ленты по 110 точек, обводка блика',
      why: 'Тонкие длинные структуры — самая строгая проверка blur: радиус виден сразу, ' +
           'а размытая кромка ленты показывает, не «съедает» ли материал мелкие элементы. ' +
           'Длинная мягкая граница хорошо показывает rim.',
      tests: ['Тонкие изогнутые структуры', 'Определённость кромки', 'Rim вдоль длинной границы'],
      make: ribbons
    },
    {
      id: 'plasma', name: 'Soft Plasma', period: 50,
      tagline: 'Медленное процедурное поле, без резких вспышек',
      tech: 'Canvas 2D, fbm 3D-шум, поле 72×46 с апскейлом',
      why: 'Среднечастотная детализация, которая не кричит. Лучшая подушка, чтобы ' +
           'сравнивать радиусы blur 0/8/14/20 px друг с другом: фон ровный, движение ' +
           'равномерное, поэтому разница в размытии не теряется.',
      tests: ['Среднечастотная детализация', 'Сравнение радиусов blur', 'Ровное движение без швов'],
      make: plasma
    },
    {
      id: 'nebula', name: 'Particle Nebula', period: 42,
      tagline: 'Мягкие светящиеся частицы с медленным движением',
      tech: 'Canvas 2D, 58 частиц + 3 туманности, radial + lighter',
      why: 'Мелкие яркие точки на тёмном фоне. Показывает, как blur и saturate ' +
           'поступают с мелкой деталью: точка должна превращаться в мягкое пятно, ' +
           'а не исчезать или превращаться в резкий штрих.',
      tests: ['Мелкая детализация', 'Saturate() на малых ярких точках', 'Точечные блики под blur'],
      make: nebula
    },
    {
      id: 'mesh', name: 'Gradient Mesh', period: 38,
      tagline: 'Крупные цветовые области, каждая движется сама',
      tech: 'Canvas 2D, 12 узлов, поле 60×40, HSV-смешивание',
      why: 'Каждый участок меняет яркость и цвет независимо. Показывает локальную ' +
           'адаптацию материала: одна панель должна оставаться читаемой, когда под ' +
           'ней одновременно и тёмный, и почти белый участок.',
      tests: ['Медленно меняющееся яркое поле', 'Адаптивность материала', 'Витровая кромка'],
      make: mesh
    },
    {
      id: 'smoke', name: 'Chromatic Smoke', period: 56,
      tagline: 'Очень медленные массы, похожие на жидкость и чернила',
      tech: 'Canvas 2D, fbm с domain warping, поле 68×44',
      why: 'Органические границы нигде не повторяются, но движение очень медленное. ' +
           'Проверяет поведение материала на непериодичной детализации и на длинных ' +
           'мягких границах — там, где rim обычно либо «съедается», либо становится ' +
           'ступенькой.',
      tests: ['Органические мягкие границы', 'Blur на фрактальной детализации', 'Rim на испаряющем плюме'],
      make: smoke
    },
    {
      id: 'stress', name: 'Glass Stress Test', period: 34,
      tagline: 'Не красота, а проверка: максимум нагрузки на материал',
      tech: 'Canvas 2D, сплошная заливка, 1px линии, шахматка, текст',
      why: 'Сцена намеренно враждебна: резкая граница чёрное/белое, чистые primaries, ' +
           '1px линии, шахматка, мелкий текст и медленный горячий блик прямо под ' +
           'стеклом. Здесь материал либо читается, либо сразу видно, где он ломается.',
      tests: ['Сравнение радиусов blur', 'Усиление насыщенности', 'Rim на чёрном и белом', 'Контраст мелкого текста'],
      probes: true,
      make: stress
    },
    {
      id: 'ramp', name: 'Luminance Ramp', period: 40,
      tagline: 'Измеритель: серый градиент 0→100, цвета нет вообще',
      tech: 'Canvas 2D, линейный градиент 11 стопов + бегущий визир',
      why: 'Чистая шкала яркости без единого оттенка. saturate() здесь бессилен: ' +
           'у серого нет насыщенности, которую можно усилить. Поэтому то, что вы ' +
           'видите, — это работа одной только рецептуры: где стекло светлеет фон, ' +
           'а где гасит его, и на какой яркости панель перестаёт отделяться.',
      tests: ['Где стекло темнит, а где светлеет', 'Точка потери отражения кромки', 'Работа без опоры на цвет'],
      diag: 'ramp',
      make: rampScene
    },
    {
      id: 'tint', name: 'Tinted Slider', period: 46,
      tagline: 'Один и тот же фон под пятью уровнями tint',
      tech: 'Canvas 2D, диагональный градиент + 2 радиальных пятна',
      why: 'Пять одинаковых панелей, различается только альфа заливки — 0.40, ' +
           '0.55, 0.68, 0.78 и 0.90, то есть весь диапазон, который теперь может ' +
           'принять единственный материал sheet. Никаких других переменных. Шкала ' +
           'неподвижна, а синяя рамка показывает, где на ней находится значение ' +
           'ползунка «Альфа sheet». Заливка здесь спокойная: карточка отвечает на ' +
           'вопрос, где начинается теряться цвет, а не выдерживает ли материал ' +
           'враждебный фон — для этого есть Sheet Sweep.',
      tests: ['Насколько 0.78 тяжелее соседних ступеней', 'Где начинается потеря цвета', 'Читаемость текста на каждой ступени'],
      diag: 'tint',
      make: tintScene
    },
    {
      id: 'rim', name: 'Rim Duel', period: 44,
      tagline: 'Наша светлая кромка против схемы iOS 27',
      tech: 'Canvas 2D, резкий разрыв тёмное/светлое + 1px блик на шве',
      why: 'Слева наша кромка — светлая обводка inset. Справа то, что Apple описал ' +
           'в iOS 27: затемнённая кромка плюс более яркий specular. Фон специально ' +
           'разделён на тёмную и светлую половину с резким швом, чтобы обе стороны ' +
           'кромки оценивались одновременно.',
      tests: ['Кромка на тёмном и на светлом', 'Читается ли силуэт без заливки', 'Блеск верхней грани'],
      diag: 'rim',
      make: rimScene
    },
    {
      id: 'fringe', name: 'Fringe Test', period: 36,
      tagline: 'Резкий край: есть ли хроматическая аберрация',
      tech: 'Canvas 2D, резкий срез чёрное/белое, primaries, линии 1px',
      why: 'Настоящая линза даёт на резком краю цветную кайму и сдвигает сам край. ' +
           'Равномерный blur не умеет ни того, ни другого — он просто размазывает ' +
           'переход. Если каймы и сдвига не видно, материал рассеивает свет, а не ' +
           'преломляет его, и называть это стеклом рано.',
      tests: ['Цветная кайма на резком краю', 'Сдвиг или только размытие', 'Поведение на чистых цветах'],
      diag: 'fringe',
      make: fringeScene
    },
    {
      id: 'lensref', name: 'Refraction Reference', period: 42,
      tagline: 'Наш blur рядом с приближением линзы',
      tech: 'Canvas 2D, сетка точек, полосы, яркая точка + SVG feDisplacementMap',
      why: 'Слева наша рецептура: текущий blur материала и saturate. Справа — ' +
           'приближение линзы на инлайновом SVG-фильтре, который гнёт подложку, ' +
           'а не размазывает её. Это не копия Apple и не реконструкция их чисел: ' +
           'это способ увидеть разницу между рассеиванием и преломлением на одном ' +
           'и том же фоне. Подпись слева следует за ползунком размытия.',
      tests: ['Изгиб сетки под стеклом', 'Концентрация света в яркой точке', 'Читаемость полос'],
      diag: 'lens',
      make: lensScene
    },
    {
      id: 'sheetchrome', name: 'Sheet Chrome', period: 46,
      tagline: 'Заголовок и сайдбар: единственное стекло на большой площади',
      tech: 'Canvas 2D, лента контента: блоки изображений, строки текста, чипы',
      why: 'Решено оставить один материал sheet на всех стеклянных поверхностях. ' +
           'Шесть из семи поверхностей маленькие — меню, тост, подсказка, список, ' +
           'модалка, кнопка, — и там 0.78 при 20 px уже проверены. Заголовок и ' +
           'сайдбар держат на себе всё стекло разом, и именно они решают, годится ' +
           'ли значение: на панели во всю высоту плотная заливка перестаёт быть ' +
           'незаметной. Геометрия и кегли здесь повторяют приложение, потому что ' +
           'проверяется не красота прямоугольника, а читаемость мелкого текста ' +
           'поверх движущегося контента.',
      tests: ['Читаются ли 11–13 px поверх ленты', 'Не выглядит ли панель мёртвой', 'Видна ли кромка на большой площади'],
      diag: 'chrome',
      make: chromeScene
    },
    {
      id: 'sheetsweep', name: 'Sheet Sweep', period: 40,
      tagline: 'Сетка альфа × blur: оба открытых параметра сразу',
      tech: 'Canvas 2D, поле точек + полосы (тот же stress, что у соседей)',
      why: 'Материал один, значит его альфа и радиус размытия — параметры одного ' +
           'материала, а не уровней разной плотности. Сетка нужна, чтобы увидеть ' +
           'пару целиком, а не по памяти после того, как каждая была выкручена ' +
           'по отдельности. Строка — blur, колонка — альфа. Ячейка, совпадающая с ' +
           'текущими ползунками, обведена: сетка должна приводить к решению, а ' +
           'не показывать девять вариантов.',
      tests: ['Где начинается потеря фона', 'Насколько 20 px отличается от 12 и 28', 'Пара, которая годится на большой площади'],
      diag: 'sweep',
      make: stress
    }
  ];

  /* --------------------------------------------------------------------
   * Loop self-check.
   *
   * A scene is only usable as a looping background if its image at phase 0
   * and at phase TAU are identical. The easy way to get this wrong is to
   * animate something with a fractional rate (sin(p * 0.6)), which drifts
   * and then jumps when the loop restarts. This returns the per-scene
   * difference so the mistake cannot come back unnoticed.
   * ------------------------------------------------------------------ */
  function loopCheck(w, h) {
    var c = document.createElement('canvas');
    c.width = w; c.height = h;
    var ctx = c.getContext('2d', { alpha: false, willReadFrequently: true });
    var out = [];
    for (var s = 0; s < SCENES.length; s++) {
      var scene = SCENES[s];
      var impl = scene.make();
      var grab = function (p) {
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        impl.draw(ctx, w, h, p);
        return ctx.getImageData(0, 0, w, h).data;
      };
      var a = grab(0), b = grab(TAU);
      var maxDiff = 0, sum = 0, n = 0;
      for (var i = 0; i < a.length; i += 4) {
        var d = Math.max(Math.abs(a[i] - b[i]),
                         Math.abs(a[i + 1] - b[i + 1]),
                         Math.abs(a[i + 2] - b[i + 2]));
        if (d > maxDiff) maxDiff = d;
        sum += d; n++;
      }
      out.push({ id: scene.id, maxDiff: maxDiff, meanDiff: +(sum / n).toFixed(3) });
    }
    return out;
  }

  /* --------------------------------------------------------------------
   * Cost measurement.
   *
   * "Which scene is heavy" is a question about real numbers, not about how
   * the code looks, so it gets measured: every scene is drawn repeatedly at
   * a representative size and the mean cost per draw is reported. Sizes
   * match a real card at DPR 2, and the field-based scenes are included
   * whole, which is exactly where the cost hides.
   * ------------------------------------------------------------------ */
  function perfCheck(w, h, iters) {
    var c = document.createElement('canvas');
    c.width = w; c.height = h;
    var ctx = c.getContext('2d', { alpha: false });
    var out = [];
    for (var s = 0; s < SCENES.length; s++) {
      var scene = SCENES[s];
      var impl = scene.make();
      /* warm up: first draw pays for lazily created gradients and buffers */
      impl.draw(ctx, w, h, 0);
      var t0 = performance.now();
      for (var i = 0; i < iters; i++) {
        impl.draw(ctx, w, h, (i / iters) * TAU);
      }
      var ms = (performance.now() - t0) / iters;
      out.push({ id: scene.id, msPerDraw: +ms.toFixed(3) });
    }
    out.sort(function (a, b) { return a.msPerDraw - b.msPerDraw; });
    return out;
  }

  /* --------------------------------------------------------------------
   * Coverage measurement.
   *
   * A background that leaves large dead-black areas is useless for judging
   * a material: there is simply nothing underneath the glass to react to.
   * This reports, per scene, the luminance floor (5th percentile) and how
   * much of the frame sits in near-black, sampled across the whole loop.
   * ------------------------------------------------------------------ */
  function coverageCheck(w, h) {
    var c = document.createElement('canvas');
    c.width = w; c.height = h;
    var ctx = c.getContext('2d', { alpha: false, willReadFrequently: true });
    var hist = new Uint32Array(256);
    var total = 0;
    var out = [];
    for (var s = 0; s < SCENES.length; s++) {
      var scene = SCENES[s];
      var impl = scene.make();
      for (var k = 0; k < 256; k++) hist[k] = 0;
      total = 0;
      var phases = [0, TAU / 4, TAU / 2, TAU * 3 / 4];
      for (var pi = 0; pi < phases.length; pi++) {
        impl.draw(ctx, w, h, phases[pi]);
        var d = ctx.getImageData(0, 0, w, h).data;
        for (var i = 0; i < d.length; i += 4) {
          var l = 0.2126 * d[i] + 0.7152 * d[i + 1] + 0.0722 * d[i + 2];
          hist[l < 0 ? 0 : (l > 255 ? 255 : l | 0)]++;
          total++;
        }
      }
      var pct = function (frac) {
        var want = total * frac, acc = 0;
        for (var b = 0; b < 256; b++) {
          acc += hist[b];
          if (acc >= want) return b;
        }
        return 255;
      };
      var below = function (thr) {
        var n = 0;
        for (var b2 = 0; b2 < thr; b2++) n += hist[b2];
        return +(100 * n / total).toFixed(1);
      };
      var sum = 0, sum2 = 0;
      for (var b3 = 0; b3 < 256; b3++) { sum += hist[b3] * b3; sum2 += hist[b3] * b3 * b3; }
      var mean = sum / total;
      out.push({
        id: scene.id,
        mean: +mean.toFixed(1),
        p05: pct(0.05), p50: pct(0.5), p95: pct(0.95),
        below16: below(16), below28: below(28)
      });
    }
    return out;
  }

  /* --------------------------------------------------------------------
   * Motion self-check.
   *
   * Liveness measured against the wall clock is unreliable. A slow scene
   * sampled near the instant its velocity passes through zero looks frozen
   * even though it is animating perfectly, and a fast one can pass while
   * stuttering. Phase is the honest clock: render each scene at four phases
   * and measure how much of the frame actually changes between them. This
   * catches a scene that ignores `p` entirely, which is the real failure.
   * ------------------------------------------------------------------ */
  function motionCheck(w, h) {
    var c = document.createElement('canvas');
    c.width = w; c.height = h;
    var ctx = c.getContext('2d', { alpha: false, willReadFrequently: true });
    var phases = [0, TAU / 4, TAU / 2, TAU * 3 / 4];
    var out = [];
    for (var s = 0; s < SCENES.length; s++) {
      var scene = SCENES[s];
      var impl = scene.make();
      var frames = [];
      for (var k = 0; k < phases.length; k++) {
        impl.draw(ctx, w, h, phases[k]);
        frames.push(ctx.getImageData(0, 0, w, h).data);
      }
      var changed = 0, total = 0;
      for (var f = 1; f < frames.length; f++) {
        var a = frames[0], b = frames[f];
        for (var i = 0; i < a.length; i += 4) {
          total++;
          if (Math.abs(a[i] - b[i]) > 1 || Math.abs(a[i + 1] - b[i + 1]) > 1
            || Math.abs(a[i + 2] - b[i + 2]) > 1) changed++;
        }
      }
      out.push({ id: scene.id, pct: +(100 * changed / total).toFixed(1) });
    }
    out.sort(function (x, y) { return x.pct - y.pct; });
    return out;
  }

  global.LGLab = {
    scenes: SCENES,
    TAU: TAU,
    loopCheck: loopCheck,
    perfCheck: perfCheck,
    coverageCheck: coverageCheck,
    motionCheck: motionCheck
  };
})(window);
