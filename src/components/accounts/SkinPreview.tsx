import { useEffect, useRef, useState } from 'react';
import type { SkinViewer } from 'skinview3d';
import { usePlayerSkin } from '../../lib/remoteImage';
import { SnakeSpinner } from '../ui/SnakeSpinner';
import { useT } from '../../lib/i18n';

const DEFAULT_WIDTH = 480;
const DEFAULT_HEIGHT = 640;
/** Room for the one-line hint under the canvas (font-xs + gap). */
const HINT_RESERVE = 26;

/**
 * One shared dynamic import for the whole module. The Accounts page is
 * remounted on every visit to the tab, and a fresh `import()` in each mount
 * re-resolved the chunk through the network stack — long enough for the
 * loading plate to be visible again on every return to the tab.
 */
let skinviewImport: Promise<typeof import('skinview3d')> | null = null;
function loadSkinview3d() {
  if (!skinviewImport) skinviewImport = import('skinview3d');
  return skinviewImport;
}

/**
 * Persistent 3D skin preview for the accounts page. The raw skin comes from
 * the same backend pipeline as the 2D avatars (cmd_get_player_head — full
 * PNG, disk-cached per uuid; offline accounts resolve to the bundled vanilla
 * default by uuid). skinview3d is loaded lazily so the heavy three.js chunk
 * is only fetched when this panel first mounts.
 *
 * The viewer instance is created ONCE and survives account switches —
 * `loadSkin` swaps the texture in place, so switching accounts never
 * flickers, teleports the camera, or restarts the model. The model is always
 * static: no automatic idle animation runs, and the render loop stays paused
 * until the user drags or zooms it. This keeps the 3D preview at full quality
 * without a continuous 60 fps WebGL draw. `cape` (a cape PNG data URL, or
 * null for no cape) is applied in place as well and survives skin swaps.
 *
 * `width`/`height` are the IDEAL (maximum) canvas size. The stage is
 * measured and the canvas scales down to it — at the default window size
 * the whole preview card fits on screen without the page scrolling.
 */
export function SkinPreview({
  uuid,
  name,
  provider,
  cape = null,
  width = DEFAULT_WIDTH,
  height = DEFAULT_HEIGHT,
}: {
  uuid: string;
  name: string;
  provider?: string;
  cape?: string | null;
  width?: number;
  height?: number;
}) {
  const t = useT();
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const viewerRef = useRef<SkinViewer | null>(null);
  // Latest desired cape. The viewer may not exist yet when the cape arrives
  // (it is created on the first ready skin), so creation reads this ref.
  const capeRef = useRef<string | null>(null);
  const { skin, status: skinStatus, refreshing } = usePlayerSkin(uuid, provider);
  const [viewerReady, setViewerReady] = useState(false);
  const [renderedUuid, setRenderedUuid] = useState<string | null>(null);
  const [viewerError, setViewerError] = useState(false);
  const [box, setBox] = useState<{ w: number; h: number } | null>(null);
  const displayReady = Boolean(viewerReady && renderedUuid === uuid);

  // Measure the stage: the canvas is sized to whatever room the card gives,
  // preserving the 3:4 ratio (height first, then width).
  const boxRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = boxRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect) setBox({ w: rect.width, h: rect.height });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const availW = Math.max(160, (box?.w ?? width) - 2);
  const availH = Math.max(160, (box?.h ?? height) - HINT_RESERVE);
  const ratio = width / height;
  const fitW = Math.min(availW, Math.min(height, availH) * ratio);
  const fitH = fitW / ratio;

  // Canvas size readable from the creation effect below (it must not re-run
  // on resize — resizing is an in-place setSize).
  const sizeRef = useRef({ w: fitW, h: fitH });
  sizeRef.current = { w: fitW, h: fitH };

  // Create the viewer on the first ready skin; later skins load in place.
  // The shared player-head store already re-fetches after an invalidation, so
  // this component never issues a second independent backend request.
  useEffect(() => {
    if (skinStatus !== 'ready' || !skin) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    let disposed = false;
    setViewerError(false);
    (async () => {
      try {
        if (!viewerRef.current) {
          const { SkinViewer } = await loadSkinview3d();
          if (disposed) return;
          const v = new SkinViewer({
            canvas,
            width: sizeRef.current.w,
            height: sizeRef.current.h,
            model: 'auto-detect',
            // No continuous RAF: the model is repainted only after a skin or
            // size change and while the user drags or zooms it.
            renderPaused: true,
          });
          v.animation = null;
          v.autoRotate = false;
          v.controls.enablePan = false;
          v.controls.addEventListener('change', () => {
            if (!v.disposed) v.render();
          });
          viewerRef.current = v;
          // The texture is loaded BEFORE the canvas is revealed. The
          // constructor draws its first frames while the skin image is still
          // decoding, so revealing right away showed an empty plate — which
          // read as a black rectangle flash on Liquid Glass. `render()` then
          // guarantees a painted frame sits in the buffer before opacity
          // flips to 1, so the fade-in never starts from a blank canvas.
          await v.loadSkin(skin, { model: 'auto-detect' });
          if (disposed) return;
          if (capeRef.current) {
            v.loadCape(capeRef.current).catch((e) =>
              console.error('cape load failed:', e)
            );
          }
          v.render();
          setViewerReady(true);
          setRenderedUuid(uuid);
        } else {
          await viewerRef.current.loadSkin(skin, { model: 'auto-detect' });
          if (disposed) return;
          viewerRef.current.render();
          setRenderedUuid(uuid);
        }
      } catch (e) {
        console.error('skin preview failed:', e);
        if (!disposed) setViewerError(true);
      }
    })();
    return () => {
      disposed = true;
    };
  }, [skin, skinStatus, uuid]);

  // Resize the existing viewer in place when the stage changes.
  useEffect(() => {
    const viewer = viewerRef.current;
    if (!viewer) return;
    viewer.setSize(fitW, fitH);
    // setSize clears the drawing buffer; a paused viewer has no next frame
    // to repaint it, so draw the resized frame explicitly.
    viewer.render();
  }, [fitW, fitH]);

  // Apply cape changes in place. Runs before the viewer exists on the very
  // first paint — capeRef covers that case at creation time.
  useEffect(() => {
    capeRef.current = cape ?? null;
    const v = viewerRef.current;
    if (!v) return;
    if (!capeRef.current) {
      v.resetCape();
      v.render();
      return;
    }
    v.loadCape(capeRef.current)
      .then(() => {
        if (!v.disposed) v.render();
      })
      .catch((e) => console.error('cape load failed:', e));
  }, [cape]);

  // Dispose once, on unmount.
  useEffect(
    () => () => {
      viewerRef.current?.dispose();
      viewerRef.current = null;
    },
    []
  );

  return (
    <div style={{ position: 'relative', flex: 1, minWidth: 0, minHeight: 0, width: '100%' }}>
      {/* Invisible sizer spanning the stage; measuring it cannot feed back
          into the canvas size (it is absolutely positioned). */}
      <div
        ref={boxRef}
        aria-hidden
        style={{ position: 'absolute', inset: 0, pointerEvents: 'none' }}
      />
      <div
        style={{
          position: 'absolute',
          inset: 0,
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
          gap: 'var(--space-sm)',
        }}
      >
        <div
          style={{
            position: 'relative',
            width: fitW,
            height: fitH,
            borderRadius: 'var(--radius-lg)',
            background: 'var(--surface-secondary, rgba(255, 255, 255, 0.04))',
            overflow: 'hidden',
            flexShrink: 0,
          }}
        >
          {/* The canvas stays mounted for the panel's whole lifetime so the
              viewer never rebinds to a detach/re-attach of the DOM node. */}
          <canvas
            ref={canvasRef}
            aria-label={name}
            style={{
              display: 'block',
              width: fitW,
              height: fitH,
              opacity: displayReady ? 1 : 0,
              transition: 'opacity 0.25s ease-out',
            }}
          />
          {!displayReady && (
            <div style={{ position: 'absolute', inset: 0 }}>
              {skinStatus === 'failed' || viewerError ? (
                <div
                  style={{
                    position: 'absolute',
                    inset: 0,
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    padding: 'var(--space-lg)',
                    textAlign: 'center',
                    fontSize: 'var(--font-size-sm)',
                    color: 'var(--text-secondary)',
                  }}
                >
                  {t('accounts.skin_viewer_unavailable')}
                </div>
              ) : null}
            </div>
          )}
        </div>
        {displayReady ? (
          <p
            style={{
              margin: 0,
              maxWidth: '100%',
              whiteSpace: 'nowrap',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              fontSize: 'var(--font-size-xs)',
              color: 'var(--text-tertiary)',
              display: 'inline-flex',
              alignItems: 'center',
              gap: 6,
            }}
          >
            {refreshing ? (
              <>
                <SnakeSpinner size={12} />
                {t('accounts.skin_updating')}
              </>
            ) : (
              t('accounts.skin_viewer_hint')
            )}
          </p>
        ) : skinStatus !== 'failed' && !viewerError ? (
          <p
            style={{
              margin: 0,
              display: 'inline-flex',
              alignItems: 'center',
              gap: 6,
              fontSize: 'var(--font-size-xs)',
              color: 'var(--text-tertiary)',
            }}
          >
            <SnakeSpinner size={12} />
            {t('accounts.skin_loading')}
          </p>
        ) : null}
      </div>
    </div>
  );
}
