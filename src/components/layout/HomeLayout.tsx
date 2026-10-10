import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { ArrowLeft } from 'lucide-react';
import { InstanceList } from '../instances/InstanceList';
import { InstanceDetail } from '../instances/InstanceDetail';
import { CreateInstanceWizard } from '../../pages/CreateInstanceWizard';
import { Modpacks } from '../../pages/Modpacks';
import { useT } from '../../lib/i18n';

// Matches --transition-fast: the shell fades out as one surface while the
// overlay fades in, and on the way back the overlay fades out while the
// shell returns — the same 150ms crossfade in both directions, so the swap
// reads as one quick movement (owner wave 26: «переход … выглядел быстрым
// и не каким-то дерганным как сейчас»). The dialog unmounts only after the
// fade, so nothing in the previous interface pops away piece by piece
// (owner wave 11: «исчезают… 1 за одним а не все сразу плавно»).
const SHELL_FADE_MS = 150;

interface HomeLayoutProps {
  onNavigate: (page: string) => void;
}

export function HomeLayout({ onNavigate }: HomeLayoutProps) {
  const t = useT();
  const [showCreate, setShowCreate] = useState(false);
  const [showModpacks, setShowModpacks] = useState(false);
  // True while the create menu should highlight the modpacks item (the
  // catalog was just opened from it; the arrow returns to this menu).
  const [viaModpacks, setViaModpacks] = useState(false);
  // Keeps the overlay mounted through its own fade-out on the way back: the
  // shell starts returning the same frame, and the two fades cross instead
  // of the catalog popping away in one frame (wave 26).
  const [overlayClosing, setOverlayClosing] = useState(false);
  const shellFadeTimer = useRef<number | null>(null);
  const overlayCloseTimer = useRef<number | null>(null);

  // "Модпаки" opens the catalog as a full-window overlay (the small menu has
  // no room for a grid). The wizard stays mounted for the duration of the
  // shell fade (it lives inside .app-layout, so it fades out with it) and
  // unmounts afterwards; the back arrow reopens the create menu.
  const openModpacks = () => {
    setViaModpacks(true);
    setShowModpacks(true);
    if (overlayCloseTimer.current) {
      // Reopened while the close fade was still running — the overlay never
      // left, so cancel its unmount and drop the closing class.
      window.clearTimeout(overlayCloseTimer.current);
      overlayCloseTimer.current = null;
    }
    setOverlayClosing(false);
    if (shellFadeTimer.current) window.clearTimeout(shellFadeTimer.current);
    shellFadeTimer.current = window.setTimeout(() => {
      shellFadeTimer.current = null;
      setShowCreate(false);
    }, SHELL_FADE_MS);
  };
  const closeModpacks = () => {
    if (shellFadeTimer.current) {
      window.clearTimeout(shellFadeTimer.current);
      shellFadeTimer.current = null;
    }
    setShowModpacks(false);
    setOverlayClosing(true);
    if (overlayCloseTimer.current) window.clearTimeout(overlayCloseTimer.current);
    overlayCloseTimer.current = window.setTimeout(() => {
      overlayCloseTimer.current = null;
      setOverlayClosing(false);
    }, SHELL_FADE_MS);
    setShowCreate(true);
    // Don't keep «Модпаки» lit after the return: the wizard reopens fresh
    // (its reset() sets the mode to «Новая») — a stuck «Модпаки» pill read
    // as a wrong active tab (owner: «выбранна как активная, хотя я нахожусь
    // уже в другой»).
    setViaModpacks(false);
  };
  const closeWizard = () => {
    setShowCreate(false);
    setViaModpacks(false);
  };
  const openWizard = () => {
    setShowCreate(true);
    setViaModpacks(false);
  };

  // Escape returns to the create menu.
  useEffect(() => {
    if (!showModpacks) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeModpacks();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showModpacks]);

  // While the overlay is open the builds page steps aside (one opacity fade
  // of the whole shell, see the CSS) so that, with the overlay transparent in
  // Liquid Glass, the raw animated wallpaper is what the modpacks UI floats
  // over — not the page behind it. Titlebar is outside .app-layout, so it
  // stays above; the overlay's own bar carries the back button.
  useEffect(() => {
    const root = document.documentElement;
    if (showModpacks) root.setAttribute('data-modpacks-open', '');
    else root.removeAttribute('data-modpacks-open');
    return () => root.removeAttribute('data-modpacks-open');
  }, [showModpacks]);

  // Drop the pending dialog unmount and overlay fade-out if the layout goes
  // away mid-fade.
  useEffect(() => () => {
    if (shellFadeTimer.current) window.clearTimeout(shellFadeTimer.current);
    if (overlayCloseTimer.current) window.clearTimeout(overlayCloseTimer.current);
  }, []);

  return (
    <div style={{ display: 'flex', height: '100%' }}>
      <InstanceList onCreateClick={openWizard} />
      <InstanceDetail onNavigate={onNavigate} />
      <CreateInstanceWizard
        open={showCreate}
        onClose={closeWizard}
        onOpenModpacks={openModpacks}
        viaModpacks={viaModpacks}
        onClearViaModpacks={() => setViaModpacks(false)}
      />

      {(showModpacks || overlayClosing) && createPortal(
        <div className={`modpacks-overlay${overlayClosing ? ' modpacks-overlay--closing' : ''}`}>
          <div className="modpacks-overlay__bar">
            <button className="btn btn--ghost" onClick={closeModpacks}>
              <ArrowLeft size={16} /> {t('common.back')}
            </button>
          </div>
          <div className="modpacks-overlay__body">
            <Modpacks />
          </div>
        </div>,
        document.body,
      )}
    </div>
  );
}