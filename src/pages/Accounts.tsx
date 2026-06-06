import { useEffect, useState } from 'react';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { useAuthStore } from '../stores/authStore';
import { useAccountsStore, type AccountEntry } from '../stores/accountsStore';
import { Button } from '../components/ui/Button';
import { Input } from '../components/ui/Input';
import { Modal } from '../components/ui/Modal';
import { MicrosoftLoginCard } from '../components/MicrosoftLoginCard';
import { addToast } from '../components/ui/Toast';
import { t } from '../lib/i18n';

/**
 * Validate an offline-account username on the frontend.
 * Mirrors the Rust rules in `cmd_add_offline_account`.
 */
function validateOfflineUsername(name: string): string | null {
  const trimmed = name.trim();
  if (!trimmed) return t('accounts.offline_username_required');
  if (trimmed.length < 3 || trimmed.length > 16) {
    return t('accounts.offline_username_invalid');
  }
  // Reject any Cyrillic block character.
  for (const ch of trimmed) {
    const code = ch.codePointAt(0) ?? 0;
    if (
      (code >= 0x0400 && code <= 0x04ff) ||
      (code >= 0x0500 && code <= 0x052f) ||
      (code >= 0x2de0 && code <= 0x2dff) ||
      (code >= 0xa640 && code <= 0xa69f)
    ) {
      return t('accounts.offline_username_cyrillic');
    }
  }
  if (!/^[A-Za-z0-9_]+$/.test(trimmed)) {
    return t('accounts.offline_username_invalid');
  }
  return null;
}

export function Accounts() {
  const { accounts, loadAccounts, addOfflineAccount, addElybyAccount, removeAccount, setDefaultAccount, changeSkin } = useAccountsStore();
  const { isLoggedIn } = useAuthStore();

  const [showAddOffline, setShowOffline] = useState(false);
  const [offlineName, setOfflineName] = useState('');
  const offlineNameError = offlineName ? validateOfflineUsername(offlineName) : null;
  const [showAddElyby, setShowElyby] = useState(false);
  const [elybyUser, setElybyUser] = useState('');
  const [elybyPass, setElybyPass] = useState('');
  const [showMicrosoftLogin, setShowMicrosoftLogin] = useState(false);

  useEffect(() => { loadAccounts(); }, [loadAccounts]);

  // Reload accounts when Microsoft login completes so the new
  // Microsoft entry shows up in the list and the modal closes.
  useEffect(() => {
    if (isLoggedIn) {
      loadAccounts();
      setShowMicrosoftLogin(false);
    }
  }, [isLoggedIn, loadAccounts]);

  const handleAddOffline = async () => {
    if (offlineNameError) {
      addToast(offlineNameError, 'error');
      return;
    }
    if (!offlineName.trim()) return;
    await addOfflineAccount(offlineName.trim());
    setOfflineName('');
    setShowOffline(false);
  };

  const handleAddElyby = async () => {
    if (!elybyUser.trim() || !elybyPass.trim()) return;
    await addElybyAccount(elybyUser.trim(), elybyPass.trim());
    setElybyUser('');
    setElybyPass('');
    setShowElyby(false);
  };

  const handleSkinChange = async (account: AccountEntry) => {
    const selected = await openFileDialog({
      title: 'Select skin file',
      filters: [{ name: 'PNG Image', extensions: ['png'] }],
      multiple: false,
    });
    if (!selected) return;

    const variant = window.prompt(t('accounts.skin_variant_prompt'), account.skin_variant || 'classic') || 'classic';
    await changeSkin(account.id, selected, variant);
    addToast(t('accounts.skin_updated_toast'), 'success');
  };

  const getTypeLabel = (t: string) => {
    switch (t) {
      case 'Microsoft': return 'Microsoft';
      case 'Offline': return 'Offline';
      case 'ElyBy': return 'Ely.by';
      default: return t;
    }
  };

  const getTypeColor = (t: string) => {
    switch (t) {
      case 'Microsoft': return 'var(--success)';
      case 'Offline': return 'var(--text-tertiary)';
      case 'ElyBy': return 'var(--warning)';
      default: return 'var(--text-tertiary)';
    }
  };

  return (
    <div className="page animate-fade-in">
      <div className="page__header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <div>
          <h1 className="page__title">{t('accounts.title')}</h1>
          <p className="page__subtitle">{t('accounts.subtitle')}</p>
        </div>
        <div style={{ display: 'flex', gap: 'var(--space-sm)' }}>
          <Button onClick={() => setShowOffline(true)}>{t('accounts.btn_add_offline')}</Button>
          <Button onClick={() => setShowElyby(true)} variant="secondary">{t('accounts.btn_add_elyby')}</Button>
          {!isLoggedIn && (
            <Button onClick={() => setShowMicrosoftLogin(true)} variant="primary">
              {t('accounts.btn_add_microsoft')}
            </Button>
          )}
        </div>
      </div>

      {/* Accounts list */}
      <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
        {accounts.map((acc) => (
          <div key={acc.id} className="glass-card" style={{
            padding: 'var(--space-lg)', display: 'flex', alignItems: 'center', gap: 'var(--space-lg)',
            border: acc.default ? '1px solid var(--primary)' : '1px solid var(--surface-border)',
          }}>
            <div style={{
              width: 40, height: 40, borderRadius: 'var(--radius-md)',
              display: 'flex', alignItems: 'center', justifyContent: 'center',
              background: 'var(--surface-glass)', fontWeight: 700, fontSize: 'var(--font-size-lg)',
              color: getTypeColor(acc.account_type),
            }}>
              {acc.name.charAt(0).toUpperCase()}
            </div>
            <div style={{ flex: 1 }}>
              <div style={{ fontWeight: 600, display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', flexWrap: 'wrap' }}>
                {acc.name}
                {acc.default && (
                  <span style={{ fontSize: 10, background: 'var(--primary)', color: 'white', padding: '2px 6px', borderRadius: 999, whiteSpace: 'nowrap' }}>
                    {t('accounts.badge_active')}
                  </span>
                )}
                {acc.account_type === 'Microsoft' && (
                  <span style={{ fontSize: 10, background: 'var(--success)', color: 'white', padding: '2px 6px', borderRadius: 999, whiteSpace: 'nowrap' }}>
                    {t('accounts.badge_licensed')}
                  </span>
                )}
              </div>
              <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
                {getTypeLabel(acc.account_type)}
                {acc.uuid && ` • ${acc.uuid.substring(0, 8)}...`}
              </div>
            </div>
            <div style={{ display: 'flex', gap: 'var(--space-xs)', flexShrink: 0 }}>
              {!acc.default && (
                <Button size="sm" variant="ghost" onClick={() => setDefaultAccount(acc.id)}>{t('accounts.btn_set_active')}</Button>
              )}
              {acc.account_type === 'Microsoft' && (
                <Button size="sm" variant="ghost" onClick={() => handleSkinChange(acc)}>{t('accounts.btn_change_skin')}</Button>
              )}
              <Button size="sm" variant="ghost" onClick={() => { removeAccount(acc.id); addToast(t('accounts.removed_toast'), 'info'); }}>{t('accounts.btn_remove')}</Button>
            </div>
          </div>
        ))}
        {accounts.length === 0 && (
          <div className="glass-card" style={{ padding: 'var(--space-xl)', textAlign: 'center', color: 'var(--text-tertiary)' }}>
            {t('accounts.empty_text')}
          </div>
        )}
      </div>

      {/* Add Offline Modal */}
      <Modal open={showAddOffline} onClose={() => setShowOffline(false)} title={t('accounts.modal_offline_title')}>
        <Input label={t('accounts.username_label')} id="offline-name" type="text" placeholder={t('accounts.username_placeholder')} value={offlineName} onChange={(e) => setOfflineName(e.target.value)} autoFocus />
        {offlineNameError && (
          <div style={{ marginTop: 6, fontSize: 'var(--font-size-xs)', color: 'var(--color-error, #ff6b6b)' }}>
            {offlineNameError}
          </div>
        )}
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setShowOffline(false)}>{t('common.cancel')}</Button>
          <Button onClick={handleAddOffline} disabled={!offlineName.trim() || !!offlineNameError}>{t('common.add')}</Button>
        </div>
      </Modal>

      {/* Add Ely.by Modal */}
      <Modal open={showAddElyby} onClose={() => setShowElyby(false)} title={t('accounts.modal_elyby_title')}>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
          <Input label={t('accounts.elyby_email_label')} id="elyby-user" type="text" placeholder={t('accounts.elyby_email_placeholder')} value={elybyUser} onChange={(e) => setElybyUser(e.target.value)} autoFocus />
          <Input label={t('accounts.password_label')} id="elyby-pass" type="password" placeholder={t('accounts.password_placeholder')} value={elybyPass} onChange={(e) => setElybyPass(e.target.value)} />
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setShowElyby(false)}>{t('common.cancel')}</Button>
          <Button onClick={handleAddElyby} disabled={!elybyUser.trim() || !elybyPass.trim()}>{t('common.add')}</Button>
        </div>
      </Modal>

      {/* Add Microsoft Modal — reuses the same login card as the main-menu "ВОЙТИ" page.
          `bare` removes the dark blurred backdrop so this dialog looks identical
          to the standalone Sign-In page, not like a "popover over dimmed content". */}
      <Modal open={showMicrosoftLogin} onClose={() => setShowMicrosoftLogin(false)} maxWidth={460} bare>
        <MicrosoftLoginCard onSuccess={() => setShowMicrosoftLogin(false)} />
      </Modal>
    </div>
  );
}
