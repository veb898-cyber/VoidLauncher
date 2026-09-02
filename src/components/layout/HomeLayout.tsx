import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { ArrowLeft } from 'lucide-react';
import { InstanceList } from '../instances/InstanceList';
import { InstanceDetail } from '../instances/InstanceDetail';
import { CreateInstanceWizard } from '../../pages/CreateInstanceWizard';
import { Modpacks } from '../../pages/Modpacks';
import { useT } from '../../lib/i18n';

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

  // "Модпаки" opens the catalog as a full-window overlay (the small menu has
  // no room for a grid). The wizard closes so its dimmed backdrop doesn't
  // swallow the titlebar; the back arrow reopens the create menu.
  const openModpacks = () => {
    setViaModpacks(true);
    setShowCreate(false);
    setShowModpacks(true);
  };
  const closeModpacks = () => {
    setShowModpacks(false);
    setShowCreate(true);
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

      {showModpacks && createPortal(
        <div className="modpacks-overlay">
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