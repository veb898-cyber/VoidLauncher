import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { useT } from '../../lib/i18n';
import { useInstanceStore } from '../../stores/instanceStore';

/**
 * User-facing fallback when Ely.by metadata/agent preparation fails. The
 * backend has already logged the technical detail; this dialog intentionally
 * stays free of authlib-injector/ALI/Yggdrasil terminology.
 */
export function ElybySkinsErrorDialog() {
  const t = useT();
  const prompt = useInstanceStore((s) => s.elybySkinsPrompt);
  const resolve = useInstanceStore((s) => s.resolveElybySkins);

  return (
    <Modal
      open={!!prompt}
      onClose={() => resolve('cancel')}
      title={t('elyby_skins.error_title')}
      maxWidth={520}
      footer={
        <div
          style={{
            display: 'flex',
            justifyContent: 'flex-end',
            gap: 'var(--space-sm)',
            flexWrap: 'wrap',
          }}
        >
          <Button variant="ghost" onClick={() => resolve('cancel')}>
            {t('common.cancel')}
          </Button>
          <Button variant="secondary" onClick={() => resolve('bypass')}>
            {t('elyby_skins.bypass')}
          </Button>
          <Button variant="primary" onClick={() => resolve('retry')}>
            {t('elyby_skins.retry')}
          </Button>
        </div>
      }
    >
      <p
        style={{
          margin: 0,
          color: 'var(--text-secondary)',
          lineHeight: 1.65,
          whiteSpace: 'pre-line',
        }}
      >
        {t('elyby_skins.error_description')}
      </p>
    </Modal>
  );
}
