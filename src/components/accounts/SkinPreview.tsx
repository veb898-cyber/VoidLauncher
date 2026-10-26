import { useEffect, useRef, useState } from 'react';
import type { SkinViewer } from 'skinview3d';
import { invoke } from '@tauri-apps/api/core';
import { IconShine } from '../../lib/remoteImage';
import { useT } from '../../lib/i18n';

const DEFAULT_WIDTH = 480;
const DEFAULT_HEIGHT = 640;
/** Room for the one-line hint under the canvas (font-xs + gap). */
const HINT_RESERVE = 26;

type SkinState =
  | { status: 'loading' }
  | { status: 'ready'; skin: string }
  | { status: 'error' };

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
 * flickers, teleports the camera, or restarts the idle animation. The model
 * is NOT auto-rotating: it stays still until the user drags it (or rotates
 * it with the wheel), exactly as asked. `cape` (a cape PNG data URL, or
 * null for no cape) is applied in place as well and survives skin swaps.
 *
 * `width`/`height` are the IDEAL (maximum) canvas size. The stage is
 * measured and the canvas scales down to it — at the default window size
 * the whole preview card fits on screen without the page scrolling.
 */
export function SkinPreview({
  uuid,
  name,
  cape = null,
  width = DEFAULT_WIDTH,
  height = DEFAULT_HEIGHT,
}: {
  uuid: string;
  name: string;
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
  const [skinState, setSkinState] = useState<SkinState>({ status: 'loading' });
  const [viewerReady, setViewerReady] = useState(false);
  const [box, setBox] = useState<{ w: number; h: number } | null>(null);

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

  // Fetch the raw skin whenever the selected account changes.
  useEffect(() => {
    if (!uuid) {
      setSkinState({ status: 'error' });
      return;
    }
    let alive = true;
    setSkinState({ status: 'loading' });
    invoke<string | null>('cmd_get_player_head', {
      uuid: uuid.replace(/-/g, '').toLowerCase(),
    })
      .then((skin) => {
        if (!alive) return;
        setSkinState(skin ? { status: 'ready', skin } : { status: 'error' });
      })
      .catch(() => {
        if (alive) setSkinState({ status: 'error' });
      });
    return () => {
      alive = false;
    };
  }, [uuid]);

  // Create the viewer on the first ready skin; later skins load in place.
  useEffect(() => {
    if (skinState.status !== 'ready') return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    let disposed = false;
    (async () => {
      try {
        if (!viewerRef.current) {
          const { SkinViewer, IdleAnimation } = await loadSkinview3d();
          if (disposed) return;
          const v = new SkinViewer({
            canvas,
            width: sizeRef.current.w,
            height: sizeRef.current.h,
            model: 'auto-detect',
          });
          // Gentle idle breathing only — no auto-rotation by default.
          const idle = new IdleAnimation();
          idle.speed = 0.35;
          v.animation = idle;
          v.autoRotate = false;
          v.controls.enablePan = false;
          viewerRef.current = v;
          // The texture is loaded BEFORE the canvas is revealed. The
          // constructor draws its first frames while the skin image is still
          // decoding, so revealing right away showed an empty plate — which
          // read as a black rectangle flash on Liquid Glass. `render()` then
          // guarantees a painted frame sits in the buffer before opacity
          // flips to 1, so the fade-in never starts from a blank canvas.
          await v.loadSkin(skinState.skin, { model: 'auto-detect' });
          if (disposed) return;
          if (capeRef.current) {
            v.loadCape(capeRef.current).catch((e) =>
              console.error('cape load failed:', e)
            );
          }
          v.render();
          setViewerReady(true);
        } else {
          await viewerRef.current.loadSkin(skinState.skin, { model: 'auto-detect' });
        }
      } catch (e) {
        console.error('skin preview failed:', e);
      }
    })();
    return () => {
      disposed = true;
    };
  }, [skinState]);

  // Resize the existing viewer in place when the stage changes.
  useEffect(() => {
    viewerRef.current?.setSize(fitW, fitH);
  }, [fitW, fitH]);

  // Apply cape changes in place. Runs before the viewer exists on the very
  // first paint — capeRef covers that case at creation time.
  useEffect(() => {
    capeRef.current = cape ?? null;
    const v = viewerRef.current;
    if (!v) return;
    if (!capeRef.current) {
      v.resetCape();
      return;
    }
    v.loadCape(capeRef.current).catch((e) =>
      console.error('cape load failed:', e)
    );
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
              opacity: viewerReady ? 1 : 0,
              transition: 'opacity 0.25s ease-out',
            }}
          />
          {!viewerReady && (
            <div style={{ position: 'absolute', inset: 0 }}>
              {skinState.status === 'error' ? (
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
              ) : (
                // No fill: the stage already sits on the card's own plate, and
                // the dark --bg-tertiary of `.icon-shine` read as a black
                // rectangle flashing on Liquid Glass. Only the light sweep is
                // kept, so loading looks the same without the dark slab.
                <IconShine style={{ position: 'absolute', inset: 0, background: 'transparent' }} />
              )}
            </div>
          )}
        </div>
        {viewerReady && (
          <p
            style={{
              margin: 0,
              maxWidth: '100%',
              whiteSpace: 'nowrap',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              fontSize: 'var(--font-size-xs)',
              color: 'var(--text-tertiary)',
            }}
          >
            {t('accounts.skin_viewer_hint')}
          </p>
        )}
      </div>
    </div>
  );
}
