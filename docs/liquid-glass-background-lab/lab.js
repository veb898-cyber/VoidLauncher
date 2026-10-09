/* ---------------------------------------------------------------------------
 * Liquid Glass background lab — catalog runner.
 *
 * Builds one card per scene, animates every visible canvas from a single
 * shared clock, and keeps a focused full-window view of any scene. Plain
 * classic script, no modules, no fetch — index.html opens from file://.
 *
 * Two rules shape the design:
 *
 * 1. One clock for every scene. A single requestAnimationFrame loop advances
 *    a shared phase, so adding scenes costs draw calls, not extra loops, and
 *    all scenes stay in step — which matters when you are comparing one
 *    material across seventeen backgrounds.
 *
 * 2. Pausing must FREEZE a frame, never blank it. A scene that has never
 *    been painted still needs a static frame while paused, otherwise a card
 *    scrolled into view under prefers-reduced-motion would be a black hole.
 * ------------------------------------------------------------------------- */

(function () {
  'use strict';

  var SCENES = window.LGLab.scenes;
  var TAU = window.LGLab.TAU;

  var reduceMotion = !!(window.matchMedia &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches);

  var MAX_DPR = 2;
  var SPEEDS = [
    { id: 'slow', label: 'Медленно', mul: 0.35 },
    { id: 'normal', label: 'Обычно', mul: 1 }
  ];

  var entries = [];
  var clock = 0;
  var last = 0;
  var running = false;
  var allPaused = reduceMotion;
  var speed = reduceMotion ? SPEEDS[0] : SPEEDS[1];
  var focusEntry = null;
  var focusOpener = null;

  /* ------------------------------------------------------------ helpers -- */

  function el(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  function two(n) { return (n < 10 ? '0' : '') + n; }

  function fitCanvas(canvas) {
    var rect = canvas.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return false;
    var dpr = Math.min(window.devicePixelRatio || 1, MAX_DPR);
    var w = Math.max(1, Math.round(rect.width * dpr));
    var h = Math.max(1, Math.round(rect.height * dpr));
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
    }
    return true;
  }

  function makeContext(canvas) {
    if (!canvas.getContext) return null;
    return canvas.getContext('2d', { alpha: false });
  }

  /* --------------------------------------------------------------- mocks -- */
  /* Deliberately not a copy of the launcher's UI: a small, fixed set of
     shapes that between them cover the cases a material has to survive —
     a large panel, a text block, a thin rail, a small button and a round
     control. They are decoration, so they never take pointer or focus. */

  /* Every mock shape uses the same material, `sheet`.
     The launcher keeps one glass material on every glass surface, so the lab
     previews exactly that: seventeen backgrounds, one recipe, and a dial per
     open parameter. Rotating a different variant per card would have measured
     a hierarchy the app no longer has. The `shell` / `float` / `tier1` classes
     stay in the stylesheet only where a card documents the old decision. */

  function mockRail() {
    var rail = el('div', 'mock mock--rail glass--sheet');
    for (var i = 0; i < 4; i++) rail.appendChild(el('span', 'mock__pip'));
    rail.setAttribute('aria-hidden', 'true');
    return rail;
  }

  function mockPanel() {
    var p = el('div', 'mock mock--panel glass--sheet');
    p.appendChild(el('div', 'mock__title', 'Панель Liquid Glass'));
    p.appendChild(el('div', 'mock__text',
      'Текстовый блок под стеклом: проверьте, читается ли он, когда под ним ' +
      'движется яркое пятно, и не меняется ли контраст на ходу.'));
    var chips = el('div', 'mock__chips');
    ['Заголовок', 'Обычный', 'Акцент', '9px'].forEach(function (label, i) {
      chips.appendChild(el('span', 'chip' +
        (i === 2 ? ' chip--accent' : '') + (i === 3 ? ' chip--tiny' : ''), label));
    });
    p.appendChild(chips);
    return p;
  }

  function mockButton() {
    var b = el('div', 'mock mock--btn glass--sheet', 'ОК');
    b.setAttribute('aria-hidden', 'true');
    return b;
  }

  function mockKnob() {
    var k = el('div', 'mock mock--knob glass--sheet');
    k.appendChild(el('span', 'mock__dot'));
    k.setAttribute('aria-hidden', 'true');
    return k;
  }

  function mockSet() {
    var set = el('div', 'mocks');
    set.appendChild(mockRail());
    set.appendChild(mockPanel());
    set.appendChild(mockButton());
    set.appendChild(mockKnob());
    return set;
  }

  /* ------------------------------------------------- diagnostic overlays ---
   * The last five cards are instruments rather than decoration. Each one
   * isolates a single variable — chroma, tint level, rim recipe, aberration,
   * lens vs scatter — so the material can be compared by eye instead of by
   * trusting a number. They replace the usual five-mock vocabulary.
   */

  function labelledColumn(caption, boxClass, capClass) {
    var col = el('div', 'diag__col');
    col.appendChild(el('div', 'diag__box ' + boxClass));
    col.appendChild(el('span', 'diag__cap' + (capClass ? ' ' + capClass : ''), caption));
    return col;
  }

  function diagRamp() {
    var set = el('div', 'diag diag--ramp');
    set.appendChild(el('div', 'diag__box diag__box--wide glass--shell'));
    var legend = el('div', 'ramp__legend');
    ['0', '25', '50', '75', '100%'].forEach(function (t) {
      legend.appendChild(el('span', null, t));
    });
    set.appendChild(legend);
    return set;
  }

  /* A fixed ruler bracketing the range `sheet` can now take, with both shipped
     values (0.78) and the previous 0.55 included. The original five steps of
     the iOS 27 slider stopped at 0.55, which was the whole of the old shell
     range; once the single material moved up to 0.78 the ruler no longer
     covered the question being asked. */
  var TINT_STEPS = [0.40, 0.55, 0.68, 0.78, 0.90];

  function diagTint() {
    var set = el('div', 'diag diag--tint');
    TINT_STEPS.forEach(function (v) {
      var col = el('div', 'diag__col');
      col.dataset.step = String(v);
      var box = el('div', 'diag__box tint__box');
      box.style.setProperty('--a', String(v));
      col.appendChild(box);
      col.appendChild(el('span', 'diag__cap', v.toFixed(2)));
      set.appendChild(col);
    });
    set.appendChild(el('span', 'diag__note',
      'шкала неподвижна; синяя рамка — текущее значение ползунка «Альфа sheet»'));
    return set;
  }

  function diagRim() {
    var set = el('div', 'diag diag--rim');
    /* both recipes are kept after the switch: the card is the record of why
       the light rim was dropped, so "ours" is now what we used to ship */
    set.appendChild(labelledColumn('было: светлая кромка', 'rim__ours glass--sheet'));
    set.appendChild(labelledColumn('стало: тёмная кромка + блик', 'rim__ios glass--sheet'));
    return set;
  }

  function diagFringe() {
    var set = el('div', 'diag diag--fringe');
    set.appendChild(el('div', 'diag__box diag__box--wide glass--sheet'));
    set.appendChild(el('span', 'diag__note',
      'настоящая линза даёт цветную кайму и сдвиг края; blur() не может'));
    return set;
  }

  function diagLens() {
    var set = el('div', 'diag diag--lens');
    /* the caption carries a number, so it follows the blur dial instead of
       going stale the moment the dial moves */
    set.appendChild(labelledColumn('наш blur 20 + saturate', 'lens__blur glass--sheet', 'cap--blur'));
    var ref = labelledColumn('линза: приближение, не копия Apple', 'lens__ref');
    set.appendChild(ref);
    return set;
  }

  /* The two surfaces that make this decision hard.
     Everything else in the app is small enough that 0.78 / 20px was already
     proven on a modal. A full-height sidebar and a full-width titlebar are the
     only places in the launcher holding this much glass at once, and a dense
     fill over a large area stops being subtle and starts looking heavy. The
     geometry and the type sizes copy the app on purpose: the thing being judged
     is 11px text staying legible over moving content. */
  var CHROME_ITEMS = ['Instances', 'Modpacks', 'Settings', 'Console'];

  function diagChrome() {
    var set = el('div', 'diag diag--chrome');

    var bar = el('div', 'chrome__titlebar glass--sheet');
    bar.appendChild(el('span', 'chrome__title', 'Instance'));
    bar.appendChild(el('span', 'chrome__chip', 'Settings'));
    set.appendChild(bar);

    var side = el('div', 'chrome__sidebar glass--sheet');
    side.appendChild(el('div', 'chrome__brand', 'VoidLauncher'));
    CHROME_ITEMS.forEach(function (label, i) {
      side.appendChild(el('div', 'chrome__item' + (i === 0 ? ' is-active' : ''), label));
    });
    set.appendChild(side);

    return set;
  }

  /* Both open numbers of the one material, on one card.
     Rows are blur, columns are alpha, so a pair can be read off directly
     instead of being dialled in and remembered. The cell matching the live
     dials is ringed — that is what makes this a decision instrument rather
     than a gallery of nine options. */
  var SWEEP_ALPHA = [0.60, 0.78, 0.90];
  var SWEEP_BLUR = [12, 20, 28];

  function diagSweep() {
    var set = el('div', 'diag diag--sweep');
    var frag = document.createDocumentFragment();

    SWEEP_BLUR.forEach(function (b) {
      SWEEP_ALPHA.forEach(function (a) {
        var cell = el('div', 'sweep__cell');
        cell.style.setProperty('--sa', a.toFixed(2));
        cell.style.setProperty('--sb', b + 'px');
        /* matched on the values themselves, not on the position in the grid:
           if the steps and the dials ever disagree, index matching would ring
           the wrong cell and say so with confidence */
        cell.dataset.alpha = a.toFixed(2);
        cell.dataset.blur = String(b);
        var lines = el('div', 'sweep__lines');
        lines.appendChild(el('span'));
        lines.appendChild(el('span'));
        lines.appendChild(el('span'));
        cell.appendChild(lines);
        cell.appendChild(el('div', 'sweep__cap', 'a ' + a.toFixed(2) + ' · b ' + b));
        frag.appendChild(cell);
      });
    });

    set.appendChild(frag);
    return set;
  }

  /* Rings the sweep cell that matches the live dials, and only that one. Both
     the card and the focus view are updated, because the focus view is a
     separate DOM tree built from the same scene. */
  function syncSweepPointer() {
    var wantA = sheetAlpha.toFixed(2);
    var wantB = String(Math.round(sheetBlur));
    var cells = document.querySelectorAll('.sweep__cell');
    for (var i = 0; i < cells.length; i++) {
      var hit = cells[i].dataset.alpha === wantA && cells[i].dataset.blur === wantB;
      cells[i].classList.toggle('is-live', hit);
    }
  }

  var DIAG = {
    ramp: diagRamp, tint: diagTint, rim: diagRim,
    fringe: diagFringe, lens: diagLens,
    chrome: diagChrome, sweep: diagSweep
  };

  function sceneOverlay(scene) {
    if (scene.diag && DIAG[scene.diag]) return DIAG[scene.diag]();
    return mockSet();
  }

  /* The lens reference needs one filter definition for the whole document, so
     the card and the focus view can both point at it without duplicating ids. */
  function injectLensFilter() {
    if (document.getElementById('lglens')) return;
    var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('class', 'diag-defs');
    svg.setAttribute('aria-hidden', 'true');
    svg.setAttribute('width', '0');
    svg.setAttribute('height', '0');
    var f = document.createElementNS('http://www.w3.org/2000/svg', 'filter');
    f.setAttribute('id', 'lglens');
    f.setAttribute('x', '-25%'); f.setAttribute('y', '-25%');
    f.setAttribute('width', '150%'); f.setAttribute('height', '150%');
    f.setAttribute('color-interpolation-filters', 'sRGB');
    /* a low-frequency noise field bends whatever passes through, which is the
       one thing a uniform blur can never do */
    f.innerHTML =
      '<feTurbulence type="fractalNoise" baseFrequency="0.009 0.015" ' +
      'numOctaves="2" seed="7" result="field"/>' +
      '<feGaussianBlur in="field" stdDeviation="2.5" result="soft"/>' +
      '<feDisplacementMap in="SourceGraphic" in2="soft" scale="30" ' +
      'xChannelSelector="R" yChannelSelector="G" result="bent"/>' +
      '<feGaussianBlur in="bent" stdDeviation="1.1"/>';
    svg.appendChild(f);
    document.body.appendChild(svg);
  }

  function probeRow() {
    var wrap = el('div', 'probes');
    [['0', 'blur 0', 'только rim'], ['8', 'blur 8', 'слабый'],
     ['14', 'blur 14', 'оболочка'], ['20', 'blur 20', 'шторка']].forEach(function (p) {
      var box = el('div', 'probe probe--' + p[0]);
      box.appendChild(el('b', null, p[1]));
      box.appendChild(el('span', null, p[2]));
      wrap.appendChild(box);
    });
    return wrap;
  }

  function stageContent(stage, scene) {
    if (scene.probes) {
      stage.classList.add('stage--probes');
      stage.appendChild(probeRow());
    }
    var canvas = el('canvas');
    canvas.setAttribute('role', 'img');
    canvas.setAttribute('aria-label',
      'Анимированный фон: ' + scene.name + '. ' + scene.tagline);
    stage.appendChild(canvas);
    stage.appendChild(el('div', 'stage__paused', 'на паузе'));
    return canvas;
  }

  /* ------------------------------------------------------------- build --- */

  function buildCard(scene, index) {
    var card = el('article', 'card');
    card.dataset.scene = scene.id;

    var head = el('div', 'card__head');
    head.appendChild(el('span', 'card__num', two(index + 1)));
    head.appendChild(el('span', 'card__name', scene.name));
    head.appendChild(el('span', 'card__tag', scene.tagline));
    card.appendChild(head);

    var stage = el('div', 'stage');
    var canvas = stageContent(stage, scene);
    stage.appendChild(sceneOverlay(scene));
    card.appendChild(stage);

    var foot = el('div', 'card__foot');
    var tests = el('ul', 'tests');
    scene.tests.forEach(function (t) { tests.appendChild(el('li', null, t)); });
    foot.appendChild(tests);

    var actions = el('div', 'card__actions');
    var playBtn = el('button', 'btn btn--sm', 'Пауза');
    playBtn.type = 'button';
    playBtn.setAttribute('aria-label', 'Пауза или запуск сцены ' + scene.name);
    var focusBtn = el('button', 'btn btn--sm', 'Фокус');
    focusBtn.type = 'button';
    focusBtn.setAttribute('aria-label', 'Открыть сцену ' + scene.name + ' на всё окно');
    actions.appendChild(playBtn);
    actions.appendChild(focusBtn);
    foot.appendChild(actions);
    card.appendChild(foot);

    card.appendChild(el('p', 'card__why', scene.why));
    card.appendChild(el('p', 'card__tech', scene.tech));

    document.getElementById('grid').appendChild(card);

    var entry = {
      scene: scene,
      card: card,
      stage: stage,
      canvas: canvas,
      ctx: makeContext(canvas),
      impl: scene.make(),
      playBtn: playBtn,
      visible: false,
      paused: allPaused,
      period: scene.period
    };
    entries.push(entry);

    playBtn.addEventListener('click', function () {
      if (entry.paused) {
        /* the clock is shared, so one card can only move if the shared
           clock is running — resuming a single card resumes everything. */
        if (allPaused) setAllPaused(false);
        else entry.paused = false;
      } else {
        entry.paused = true;
      }
      syncPlayButton(entry);
      kick();
    });
    focusBtn.addEventListener('click', function () { openFocus(entry, focusBtn); });

    syncPlayButton(entry);
    return entry;
  }

  function syncPlayButton(entry) {
    entry.playBtn.textContent = entry.paused ? 'Старт' : 'Пауза';
    entry.card.classList.toggle('is-paused', entry.paused);
  }

  /* -------------------------------------------------------------- focus -- */

  function buildFocusStage(scene) {
    var stage = document.getElementById('focusStage');
    stage.innerHTML = '';
    stage.className = 'focus__stage' + (scene.probes ? ' stage--probes' : '');
    var canvas = stageContent(stage, scene);
    stage.appendChild(sceneOverlay(scene));
    /* a freshly built ruler starts unmarked, so the pointers have to be applied
       again — otherwise the large view silently loses them while the small card
       keeps them, and the two disagree about the same numbers */
    syncTintPointer();
    syncSweepPointer();
    return {
      scene: scene,
      stage: stage,
      canvas: canvas,
      ctx: makeContext(canvas),
      impl: scene.make(),
      visible: true,
      paused: allPaused,
      period: scene.period
    };
  }

  function openFocus(entry, opener) {
    var scene = entry.scene;
    document.getElementById('focusTitle').textContent = scene.name;
    document.getElementById('focusNote').textContent = scene.tagline;
    document.getElementById('focusWhy').textContent = scene.why;
    focusOpener = opener || null;
    var slot = buildFocusStage(scene);
    slot.canvas.style.visibility = 'hidden';
    var overlay = document.getElementById('focus');
    overlay.classList.add('is-open');
    overlay.setAttribute('aria-hidden', 'false');
    focusEntry = slot;
    /* one frame after layout, so the canvas finally has a measurable size */
    requestAnimationFrame(function () {
      fitCanvas(slot.canvas);
      slot.canvas.style.visibility = '';
      render(slot, true);
      kick();
      var close = document.getElementById('focusClose');
      if (close) close.focus();
    });
  }

  function closeFocus() {
    var overlay = document.getElementById('focus');
    if (document.fullscreenElement) {
      if (document.exitFullscreen) document.exitFullscreen();
    }
    overlay.classList.remove('is-open');
    overlay.setAttribute('aria-hidden', 'true');
    focusEntry = null;
    if (focusOpener && document.contains(focusOpener)) focusOpener.focus();
    focusOpener = null;
  }

  function setFullscreen(on) {
    var overlay = document.getElementById('focus');
    var btn = document.getElementById('focusFull');
    if (on) {
      var req = overlay.requestFullscreen || overlay.webkitRequestFullscreen;
      if (!req) return;
      var p = req.call(overlay);
      if (p && p.catch) p.catch(function () { btn.disabled = true; });
    } else if (document.exitFullscreen) {
      document.exitFullscreen();
    }
  }

  function syncFullscreenBtn() {
    var btn = document.getElementById('focusFull');
    if (!btn) return;
    btn.textContent = document.fullscreenElement ? 'Выйти из полного экрана' : 'Во весь экран';
    /* entering or leaving fullscreen resizes the stage without a window resize */
    if (focusEntry) sizeAndRender(focusEntry, true);
  }

  /* ------------------------------------------------------------- render -- */

  /* force = paint a still frame even while paused, so nothing is ever blank */
  function render(entry, force) {
    if (!entry || !entry.ctx) return;
    if (entry.paused && !force) return;
    var canvas = entry.canvas;
    var w = canvas.width, h = canvas.height;
    if (!w || !h) return;
    var phase = ((clock / entry.period) % 1) * TAU;
    entry.ctx.save();
    try {
      entry.impl.draw(entry.ctx, w, h, phase);
    } finally {
      entry.ctx.restore();
    }
  }

  /* a canvas that has just been resized still shows stale pixels */
  function sizeAndRender(entry, force) {
    if (fitCanvas(entry.canvas)) {
      var ctx = entry.ctx = makeContext(entry.canvas);
      if (ctx) render(entry, force);
      return true;
    }
    return false;
  }

  function frame(now) {
    if (!last) last = now;
    var dt = Math.min((now - last) / 1000, 0.1);
    last = now;
    if (!allPaused) clock += dt * speed.mul;

    for (var i = 0; i < entries.length; i++) {
      var e = entries[i];
      if (!e.visible) continue;
      render(e);
    }
    if (focusEntry) render(focusEntry);

    if (running) requestAnimationFrame(frame);
  }

  function kick() {
    if (running) return;
    running = true;
    last = 0;
    requestAnimationFrame(frame);
  }

  function stop() { running = false; }

  /* -------------------------------------------------------------- wiring - */

  function setAllPaused(next) {
    allPaused = next;
    var btn = document.getElementById('pauseAll');
    btn.textContent = allPaused ? 'Запустить все' : 'Пауза для всех';
    btn.setAttribute('aria-pressed', String(allPaused));
    document.body.classList.toggle('is-paused', allPaused);
    entries.forEach(function (e) {
      e.paused = allPaused;
      syncPlayButton(e);
    });
    if (focusEntry) {
      focusEntry.paused = allPaused;
      /* freeze on the current frame, and never leave the view empty */
      render(focusEntry, true);
    }
    if (allPaused) stop(); else kick();
  }

  function setSpeed(next) {
    speed = next;
    document.body.dataset.speed = next.id;
    [].forEach.call(document.querySelectorAll('.speed button'), function (b) {
      var on = b.dataset.speed === next.id;
      b.setAttribute('aria-pressed', String(on));
    });
    if (!allPaused) kick();
  }

  /* ------------------------------------------------ transparency control ---
   * Apple ships transparency as a range and pairs it with the accessibility
   * adaptations, so the lab drives the same range instead of one chosen number.
   *
   * Only the fill alphas move. The rim deliberately does not: in the real
   * material the edge is a specular highlight rather than a veil, and it is the
   * one thing that has to survive when the fill goes fully clear — which is
   * exactly why Apple darkened the edge in iOS 27. Holding it fixed here is what
   * makes that visible at the transparent end of the scale.
   */

  /* The one material's fill, as the app writes it: `rgb(20, 24, 34)` is the
     sheet colour, not the old shell `rgb(22, 27, 38)`. Getting this wrong is
     invisible on a swatch and obvious on a large panel. */
  var FILL_RGB = '20, 24, 34';

  /* Tint is NOT on a dial. In the app `--lg-tint` drives hover and active
     states, and it stays at the value it ships with. Tying it to the fill
     alpha looked reasonable while the alpha lived at 0.30, but at 0.78 it
     would turn every chip and pip into a near-opaque white blob — a second,
     unmeasured variable changing underneath the one being measured. */
  var TINT_ALPHA = 0.16;
  var TINT_RGB = '226, 236, 252';

  /* The launcher keeps one glass material, so the only numbers still open are
     that material's own alpha and blur radius. Both defaults are what the app
     ships today on `.modal`, which is the one surface those values were ever
     actually checked on — the titlebar and sidebar are the untested case, and
     they are what cards 16 and 17 exist for. */
  var sheetAlpha = 0.78;
  var sheetBlur = 20;
  var BASE_SAT = 1.7;

  var contrastOn = false;
  var denseOn = false;

  var alphaRangeEl = null;
  var alphaValueEl = null;
  var blurRangeEl = null;
  var blurValueEl = null;

  function alphaColor(parts, a) {
    return 'rgb(' + parts + ', ' + a.toFixed(2) + ')';
  }

  /* One entry point for all four controls, so they cannot end up fighting over
     the same token with only the last click honoured. */
  function applyMaterial() {
    var root = document.documentElement;
    var a = sheetAlpha;
    var flat = contrastOn || denseOn;

    /* "Reduce transparency" is what the app already does: opaque, no blur */
    if (denseOn) a = 1;

    root.style.setProperty('--fill-sheet', alphaColor(FILL_RGB, a));
    root.style.setProperty('--tint', alphaColor(TINT_RGB, TINT_ALPHA));
    root.style.setProperty('--blur-sheet', flat ? '0px' : Math.round(sheetBlur) + 'px');
    root.style.setProperty('--sat-sheet', flat ? '1' : BASE_SAT);

    document.body.classList.toggle('is-high-contrast', contrastOn);

    if (alphaValueEl) {
      alphaValueEl.textContent = a.toFixed(2);
      alphaValueEl.title = denseOn ? '«Плотный» переопределяет ползунок' : '';
    }
    if (blurValueEl) {
      blurValueEl.textContent = (flat ? '0' : Math.round(sheetBlur)) + 'px';
      blurValueEl.title = flat ? 'Контраст и «Плотный» обнуляют размытие' : '';
    }
    if (alphaRangeEl) alphaRangeEl.disabled = denseOn;
    if (blurRangeEl) blurRangeEl.disabled = denseOn;

    /* every lens caption in the document, card and focus copy alike */
    var caps = document.querySelectorAll('.cap--blur');
    for (var c = 0; c < caps.length; c++) {
      caps[c].textContent = 'наш blur ' + (flat ? '0' : Math.round(sheetBlur)) + ' + saturate';
    }

    syncTintPointer();
    syncSweepPointer();
  }

  /* The tint card is a fixed ruler, so only the pointer travels. Every ruler in
     the document — the card and the rebuilt focus copy alike — has to be marked,
     and the match is made on the step VALUE rather than its position: the query
     returns one flat list, so an index would only ever match the first ruler and
     the focus view would quietly disagree with the card it came from. */
  function syncTintPointer() {
    var cols = document.querySelectorAll('.diag--tint .diag__col');
    var bestStep = null;
    var bestD = Infinity;
    var i;
    for (i = 0; i < cols.length; i++) {
      var d = Math.abs(parseFloat(cols[i].dataset.step) - sheetAlpha);
      if (d < bestD) { bestD = d; bestStep = parseFloat(cols[i].dataset.step); }
    }
    for (i = 0; i < cols.length; i++) {
      cols[i].classList.toggle('is-current',
        bestStep !== null && parseFloat(cols[i].dataset.step) === bestStep);
    }
  }

  function wireMaterial() {
    alphaRangeEl = document.getElementById('sheetAlpha');
    alphaValueEl = document.getElementById('sheetAlphaValue');
    blurRangeEl = document.getElementById('sheetBlur');
    blurValueEl = document.getElementById('sheetBlurValue');

    if (alphaRangeEl) {
      alphaRangeEl.addEventListener('input', function () {
        sheetAlpha = parseFloat(alphaRangeEl.value) || 0;
        applyMaterial();
      });
    }

    if (blurRangeEl) {
      blurRangeEl.addEventListener('input', function () {
        sheetBlur = parseFloat(blurRangeEl.value) || 0;
        applyMaterial();
      });
    }

    [{ id: 'toggleContrast', state: 'contrast' },
     { id: 'toggleDense', state: 'dense' }].forEach(function (pair) {
      var btn = document.getElementById(pair.id);
      if (!btn) return;
      btn.addEventListener('click', function () {
        if (pair.state === 'contrast') contrastOn = !contrastOn;
        else denseOn = !denseOn;
        btn.setAttribute('aria-pressed',
          String(pair.state === 'contrast' ? contrastOn : denseOn));
        applyMaterial();
      });
    });

    applyMaterial();
  }

  function wire() {
    SCENES.forEach(buildCard);
    /* after the cards, so the tint ruler exists for the pointer to land on */
    wireMaterial();

    /* paint one still frame of every scene, so nothing is blank on arrival
       even when the lab starts paused */
    entries.forEach(function (e) { sizeAndRender(e, true); });

    /* only animate what is on screen */
    if ('IntersectionObserver' in window) {
      var io = new IntersectionObserver(function (records) {
        records.forEach(function (rec) {
          var entry = rec.target.__entry;
          if (!entry) return;
          entry.visible = rec.isIntersecting;
          if (rec.isIntersecting) {
            sizeAndRender(entry, true);
            if (!allPaused) kick();
          }
        });
      }, { rootMargin: '200px' });
      entries.forEach(function (e) {
        e.stage.__entry = e;
        io.observe(e.stage);
      });
    } else {
      entries.forEach(function (e) { e.visible = true; });
    }

    window.addEventListener('resize', function () {
      entries.forEach(function (e) { if (e.visible) sizeAndRender(e, true); });
      if (focusEntry) sizeAndRender(focusEntry, true);
    });

    document.getElementById('pauseAll')
      .addEventListener('click', function () { setAllPaused(!allPaused); });

    var speedBox = document.querySelector('.speed');
    SPEEDS.forEach(function (s) {
      var b = el('button', 'btn btn--sm', s.label);
      b.type = 'button';
      b.dataset.speed = s.id;
      b.addEventListener('click', function () { setSpeed(s); });
      speedBox.appendChild(b);
    });
    setSpeed(speed);

    var toggleSamples = document.getElementById('toggleSamples');
    toggleSamples.addEventListener('click', function () {
      var hidden = document.body.classList.toggle('no-samples');
      toggleSamples.setAttribute('aria-pressed', String(hidden));
      toggleSamples.textContent = hidden ? 'Показать панели' : 'Скрыть панели';
    });

    document.getElementById('focusClose').addEventListener('click', closeFocus);
    document.getElementById('focusFull').addEventListener('click', function () {
      setFullscreen(!document.fullscreenElement);
    });
    document.addEventListener('fullscreenchange', syncFullscreenBtn);
    document.getElementById('focus').addEventListener('click', function (ev) {
      if (ev.target === ev.currentTarget) closeFocus();
    });
    document.addEventListener('keydown', function (ev) {
      if (ev.key === 'Escape' && focusEntry && !document.fullscreenElement) closeFocus();
    });

    var pauseAll = document.getElementById('pauseAll');
    pauseAll.setAttribute('aria-pressed', String(allPaused));
    pauseAll.textContent = allPaused ? 'Запустить все' : 'Пауза для всех';
    document.body.classList.toggle('is-paused', allPaused);

    if (!document.fullscreenEnabled) {
      var full = document.getElementById('focusFull');
      if (full) {
        full.disabled = true;
        full.title = 'Браузер не поддерживает полный экран';
      }
    }
    syncFullscreenBtn();

    if (!allPaused) kick();
  }

  /* One DOM read tells a headless check everything the four controls write,
     including the rim they must leave alone. */
  function materialSnapshot() {
    var cs = getComputedStyle(document.documentElement);
    function tok(name) { return cs.getPropertyValue(name).trim(); }
    return {
      alpha: sheetAlpha,
      blur: sheetBlur,
      dense: denseOn,
      contrast: contrastOn,
      fillSheet: tok('--fill-sheet'),
      blurSheet: tok('--blur-sheet'),
      satSheet: tok('--sat-sheet'),
      tint: tok('--tint'),
      rim: tok('--rim'),
      rimTop: tok('--rim-top')
    };
  }

  /* Runs only on ?selftest=1 and reports into the DOM, so a headless browser
     can read the result without needing access to this script's scope. */
  function runSelfTest() {
    var out = document.getElementById('selftest');
    if (!out) return;
    try {
      out.textContent = JSON.stringify({
        material: materialSnapshot(),
        loop: window.LGLab.loopCheck(200, 130),
        /* 790x490 is a real card canvas at DPR 2 */
        perf: window.LGLab.perfCheck(790, 490, 40),
        coverage: window.LGLab.coverageCheck(240, 150),
        /* phase-based, so a slow scene cannot look frozen by accident */
        motion: window.LGLab.motionCheck(240, 150)
      });
    } catch (err) {
      out.textContent = JSON.stringify({ error: String(err) });
    }
  }

  function boot() {
    injectLensFilter();
    wire();
    if (/[?&]selftest=1/.test(window.location.search)) runSelfTest();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})();
