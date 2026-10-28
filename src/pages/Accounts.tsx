import { useEffect, useMemo, useRef, useState } from 'react';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { openUrl } from '@tauri-apps/plugin-opener';
import { useAccountsStore, type AccountEntry } from '../stores/accountsStore';
import { Button } from '../components/ui/Button';
import { Input } from '../components/ui/Input';
import { Modal } from '../components/ui/Modal';
import { CustomSelect } from '../components/ui/CustomSelect';
import { Tooltip } from '../components/ui/Tooltip';
import { ConfirmDialog } from '../components/ui/ConfirmDialog';
import { EmptyState } from '../components/ui/EmptyState';
import { SnakeSpinner } from '../components/ui/SnakeSpinner';
import { MicrosoftLoginCard } from '../components/MicrosoftLoginCard';
import { SkinPreview } from '../components/accounts/SkinPreview';
import { addToast } from '../components/ui/Toast';
import { Ban, Check, RefreshCw, LogOut, Plus, Shirt, UserRoundPlus, HelpCircle } from 'lucide-react';
import { t } from '../lib/i18n';
import { Avatar } from '../lib/remoteImage';

const ELYBY_SKINS_URL = 'https://ely.by/skins';

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
  const {
    accounts,
    loadAccounts,
    addOfflineAccount,
    addElybyAccount,
    removeAccount,
    setDefaultAccount,
    changeSkin,
    fetchCapes,
    applyCapes,
    setPlayerCape,
  } = useAccountsStore();
  const capesByAccount = useAccountsStore((s) => s.capesByAccount);
  const capesLoadingByAccount = useAccountsStore((s) => s.capesLoadingByAccount);
  const capesErrorByAccount = useAccountsStore((s) => s.capesErrorByAccount);

  const [showAddOffline, setShowOffline] = useState(false);
  const [offlineName, setOfflineName] = useState('');
  const offlineNameError = offlineName ? validateOfflineUsername(offlineName) : null;
  const [showAddElyby, setShowElyby] = useState(false);
  const [elybyUser, setElybyUser] = useState('');
  const [elybyPass, setElybyPass] = useState('');
  const [showMicrosoftLogin, setShowMicrosoftLogin] = useState(false);
  const [skinModalAccount, setSkinModalAccount] = useState<AccountEntry | null>(null);
  const [skinModalPath, setSkinModalPath] = useState('');
  const [skinVariant, setSkinVariant] = useState<'classic' | 'slim'>('classic');
  const [showElybySkinHelp, setShowElybySkinHelp] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [switching, setSwitching] = useState(false);
  const [capeSaving, setCapeSaving] = useState(false);
  const [addingOffline, setAddingOffline] = useState(false);
  const [addingElyby, setAddingElyby] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<AccountEntry | null>(null);
  const [removing, setRemoving] = useState(false);

  useEffect(() => { loadAccounts(); }, [loadAccounts]);

  // Effective selection: the clicked account, else the active one, else the
  // first in the list. Derived (not stored) so account list changes (add /
  // remove) always leave a valid selection.
  const selected = useMemo(
    () =>
      accounts.find((a) => a.id === selectedId) ??
      accounts.find((a) => a.default) ??
      accounts[0] ??
      null,
    [accounts, selectedId]
  );

  // Latest selection, readable from async callbacks that outlive a render
  // (a slow cape switch must not overwrite the state of another account).
  const selectedIdRef = useRef<string | null>(null);
  selectedIdRef.current = selected?.id ?? null;

  // Cape lists live in the accounts store, keyed by account id. Selecting an
  // account therefore paints its capes (and the worn cape on the 3D model)
  // synchronously from the shared cache, while a stale entry is refreshed in
  // the background without ever blanking what is already on screen.
  const selectedCapeAccountId =
    selected && selected.account_type === 'Microsoft' && selected.has_ms_session !== false
      ? selected.id
      : null;
  const capes = selectedCapeAccountId
    ? capesByAccount[selectedCapeAccountId]?.capes ?? []
    : [];
  const capesLoading = selectedCapeAccountId
    ? !!capesLoadingByAccount[selectedCapeAccountId]
    : false;
  const capesError = selectedCapeAccountId
    ? capesErrorByAccount[selectedCapeAccountId] ?? null
    : null;

  useEffect(() => {
    if (selectedCapeAccountId) fetchCapes(selectedCapeAccountId);
  }, [selectedCapeAccountId, fetchCapes]);

  const retryCapes = () => {
    if (selectedCapeAccountId) fetchCapes(selectedCapeAccountId, true);
  };

  const activeCapePng = capes.find((c) => c.active && c.png)?.png ?? null;

  const handleAddOffline = async () => {
    if (offlineNameError) {
      addToast(offlineNameError, 'error');
      return;
    }
    if (!offlineName.trim() || addingOffline) return;
    setAddingOffline(true);
    try {
      await addOfflineAccount(offlineName.trim());
      setOfflineName('');
      setShowOffline(false);
    } catch (e) {
      addToast(t('accounts.add_failed', { service: 'Offline', error: String(e) }), 'error');
    } finally {
      setAddingOffline(false);
    }
  };

  const handleAddElyby = async () => {
    if (!elybyUser.trim() || !elybyPass.trim() || addingElyby) return;
    setAddingElyby(true);
    try {
      await addElybyAccount(elybyUser.trim(), elybyPass.trim());
      setElybyUser('');
      setElybyPass('');
      setShowElyby(false);
    } catch (e) {
      addToast(t('accounts.add_failed', { service: 'Ely.by', error: String(e) }), 'error');
    } finally {
      setAddingElyby(false);
    }
  };

  const handleMakeActive = async () => {
    if (!selected || selected.default || switching) return;
    setSwitching(true);
    try {
      await setDefaultAccount(selected.id);
    } finally {
      setSwitching(false);
    }
  };

  const handleRemoveAccount = async (account: AccountEntry) => {
    if (removing) return;
    setRemoving(true);
    try {
      await removeAccount(account.id);
      if (selectedId === account.id) setSelectedId(null);
      setRemoveTarget(null);
      addToast(t('accounts.removed_toast'), 'success');
    } catch (err) {
      addToast(t('accounts.removed_error', { error: String(err) }), 'error');
    } finally {
      setRemoving(false);
    }
  };

  // Cape switching is optimistic: the worn marker moves instantly and the
  // request runs in the background. Success only confirms the server list
  // (no toast — the picker itself shows the result); failure rolls the
  // marker back and surfaces the error.
  const handleSetCape = async (capeId: string | null) => {
    if (!selected || capeSaving) return;
    const accountId = selected.id;
    const prev = capes;
    applyCapes(accountId, prev.map((c) => ({ ...c, active: capeId !== null && c.id === capeId })));
    setCapeSaving(true);
    try {
      const list = await setPlayerCape(accountId, capeId);
      if (selectedIdRef.current !== accountId) return;
      // The backend answers with metadata only (textures are unchanged);
      // keep the PNGs we already have, keyed by cape id.
      const pngById = new Map(prev.map((c) => [c.id, c.png]));
      applyCapes(accountId, list.map((c) => ({ ...c, png: c.png || pngById.get(c.id) || '' })));
    } catch (e) {
      if (selectedIdRef.current === accountId) applyCapes(accountId, prev);
      addToast(t('accounts.cape_update_error', { error: String(e) }), 'error');
    } finally {
      setCapeSaving(false);
    }
  };

  const handleSkinChange = async (account: AccountEntry) => {
    const selectedFile = await openFileDialog({
      title: t('accounts.skin_file_title'),
      filters: [{ name: t('accounts.filter_png'), extensions: ['png'] }],
      multiple: false,
    });
    if (!selectedFile) return;

    setSkinModalAccount(account);
    setSkinModalPath(selectedFile);
    setSkinVariant((account.skin_variant || 'classic') as 'classic' | 'slim');
  };

  const confirmSkinChange = async () => {
    if (!skinModalAccount) return;
    try {
      await changeSkin(skinModalAccount.id, skinModalPath, skinVariant);
      addToast(t('accounts.skin_updated_toast'), 'success');
      setSkinModalAccount(null);
      setSkinModalPath('');
    } catch (e) {
      addToast(t('accounts.skin_update_error', { error: String(e) }), 'error');
    }
  };

  const getTypeLabel = (type: string) => {
    switch (type) {
      case 'Microsoft': return 'Microsoft';
      case 'Offline': return t('settings.account_type_offline');
      case 'ElyBy': return 'Ely.by';
      default: return type;
    }
  };

  return (
    <div className="page page--accounts">
      <div className="page__header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <div>
          <h1 className="page__title">{t('accounts.title')}</h1>
          <p className="page__subtitle">{t('accounts.subtitle')}</p>
        </div>
        <div style={{ display: 'flex', gap: 'var(--space-sm)', flexWrap: 'wrap' }}>
          {/* Three peer ways to add an account — same weight, so none of them
              silently outranks the others. */}
          <Button onClick={() => setShowOffline(true)} variant="secondary">{t('accounts.btn_add_offline')}</Button>
          <Button onClick={() => setShowElyby(true)} variant="secondary">{t('accounts.btn_add_elyby')}</Button>
          <Button onClick={() => setShowMicrosoftLogin(true)} variant="secondary">
            {t('accounts.btn_add_microsoft')}
          </Button>
        </div>
      </div>

      {accounts.length === 0 ? (
        <div className="glass-card" style={{ padding: 'var(--space-lg)' }}>
          <EmptyState compact icon={<UserRoundPlus size={24} />} title={t('accounts.empty_text')} />
        </div>
      ) : (
        /* Big skin first (left), account management on the right: the preview
           owns the leftover height and scales down to it, so at the default
           window size the whole page fits without scrolling. */
        <div className="accounts-layout">
          <div className="glass-card accounts-preview">
            {selected ? (
              <>
                <div className="accounts-preview__stage">
                  <SkinPreview
                    uuid={selected.uuid ?? ''}
                    name={selected.name}
                    cape={selected.account_type === 'Microsoft' ? activeCapePng : null}
                  />
                </div>
                <div style={{ textAlign: 'center', display: 'flex', flexDirection: 'column', gap: 4, flexShrink: 0 }}>
                  <div style={{ fontWeight: 700, fontSize: 'var(--font-size-lg)' }}>{selected.name}</div>
                  <div style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
                    {getTypeLabel(selected.account_type)}
                    {selected.uuid ? ` • ${selected.uuid.substring(0, 8)}...` : ''}
                  </div>
                  {selected.default && (
                    <div style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)' }}>
                      {t('accounts.active_for_launch')}
                    </div>
                  )}
                </div>
              </>
            ) : (
              <EmptyState compact icon={<UserRoundPlus size={24} />} title={t('accounts.empty_text')} />
            )}
          </div>

          <div className="accounts-rail">
            <div className="accounts-list">
              {accounts.map((acc) => {
                const isSelected = selected?.id === acc.id;
                return (
                  <div
                    key={acc.id}
                    className="glass-card account-card"
                    role="button"
                    tabIndex={0}
                    aria-pressed={isSelected}
                    onClick={() => setSelectedId(acc.id)}
                    onKeyDown={(e) => {
                      if (e.target !== e.currentTarget) return;
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        setSelectedId(acc.id);
                      }
                    }}
                    style={{
                      /* Tightened side padding/gaps (12px, 8px, 6px) purely to
                         free row width: with both badges (АКТИВЕН +
                         ЛИЦЕНЗИЯ) the name line needed ~17px more than the
                         380px rail could give and wrapped to a second line. */
                      padding: 'var(--space-md)', display: 'flex', alignItems: 'center', gap: 'var(--space-sm)',
                      border: isSelected ? '1px solid var(--primary)' : '1px solid var(--surface-border)',
                    }}
                  >
                    <Avatar uuid={acc.uuid ?? ''} name={acc.name} size={36} />
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ fontWeight: 600, display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
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
                        {acc.account_type === 'Microsoft' && acc.has_ms_session === false && (
                          <Tooltip content={t('accounts.needs_login_hint')}>
                            <span
                              style={{ fontSize: 10, background: 'var(--warning)', color: 'white', padding: '2px 6px', borderRadius: 999, whiteSpace: 'nowrap' }}
                            >
                              {t('accounts.badge_needs_login')}
                            </span>
                          </Tooltip>
                        )}
                      </div>
                      <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>
                        {getTypeLabel(acc.account_type)}
                        {acc.uuid && ` • ${acc.uuid.substring(0, 8)}...`}
                      </div>
                    </div>
                    {/* Per-account actions live on the card itself — the same
                        quick icon row the list had before the redesign. */}
                    <div style={{ display: 'flex', gap: 'var(--space-xs)', flexShrink: 0 }}>
                      {acc.account_type === 'Microsoft' && (
                        <Tooltip content={t('accounts.btn_change_skin')}>
                          <Button
                            size="sm"
                            variant="ghost"
                            aria-label={t('accounts.btn_change_skin')}
                            onClick={(e) => { e.stopPropagation(); handleSkinChange(acc); }}
                          >
                            <Shirt size={14} />
                          </Button>
                        </Tooltip>
                      )}
                      {acc.account_type === 'ElyBy' && (
                        <Tooltip content={t('accounts.elyby_skin_help_btn')}>
                          <Button
                            size="sm"
                            variant="ghost"
                            aria-label={t('accounts.elyby_skin_help_btn')}
                            onClick={(e) => { e.stopPropagation(); setShowElybySkinHelp(true); }}
                          >
                            <HelpCircle size={14} />
                          </Button>
                        </Tooltip>
                      )}
                      <Tooltip content={t('accounts.remove_btn')}>
                        <Button
                          size="sm"
                          variant="ghost"
                          aria-label={t('accounts.remove_btn')}
                          onClick={(e) => { e.stopPropagation(); setRemoveTarget(acc); }}
                        >
                          <LogOut size={14} />
                        </Button>
                      </Tooltip>
                    </div>
                  </div>
                );
              })}
            </div>

            {selected && !selected.default && (
              <div className="glass-card" style={{ padding: 'var(--space-md) var(--space-lg)', flexShrink: 0 }}>
                <Button onClick={handleMakeActive} disabled={switching} style={{ width: '100%' }}>
                  <Check size={14} style={{ marginRight: 6 }} />
                  {t('accounts.make_active')}
                </Button>
              </div>
            )}

            {selected?.account_type === 'Microsoft' && selected.has_ms_session !== false && (
              <div className="glass-card" style={{ padding: 'var(--space-lg)', flexShrink: 0 }}>
                <div style={{ fontWeight: 600, marginBottom: 'var(--space-sm)' }}>
                  {t('accounts.capes_title')}
                </div>
                {capesError && capes.length > 0 && (
                  <div style={{ marginBottom: 'var(--space-sm)', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                    {t('accounts.capes_refresh_error', { error: capesError })}
                  </div>
                )}
                {capesLoading && capes.length === 0 ? (
                  <div style={{ display: 'flex', justifyContent: 'center', padding: 'var(--space-md)' }}>
                    <SnakeSpinner size={18} />
                  </div>
                ) : capesError && capes.length === 0 ? (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)', alignItems: 'flex-start' }}>
                    <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
                      {t('accounts.capes_load_error', { error: capesError })}
                    </div>
                    <Button size="sm" variant="secondary" onClick={retryCapes}>
                      <RefreshCw size={14} style={{ marginRight: 6 }} />
                      {t('common.retry')}
                    </Button>
                  </div>
                ) : (
                  <div style={{ display: 'flex', flexWrap: 'wrap', gap: 'var(--space-sm)' }}>
                    <button
                      type="button"
                      className={`cape-tile${!capes.some((c) => c.active) ? ' cape-tile--active' : ''}`}
                      onClick={() => handleSetCape(null)}
                      disabled={capeSaving}
                      title={t('accounts.cape_none')}
                    >
                      <span className="cape-thumb cape-thumb--none"><Ban size={18} /></span>
                      <span className="cape-alias">{t('accounts.cape_none')}</span>
                    </button>
                    {capes.map((c) => (
                      <button
                        key={c.id}
                        type="button"
                        className={`cape-tile${c.active ? ' cape-tile--active' : ''}`}
                        onClick={() => { if (!c.active) handleSetCape(c.id); }}
                        disabled={capeSaving}
                        title={c.alias}
                      >
                        {c.png ? (
                          <span className="cape-thumb" style={{ backgroundImage: `url(${c.png})` }} />
                        ) : (
                          <span className="cape-thumb" />
                        )}
                        <span className="cape-alias">{c.alias || '—'}</span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      )}

      {/* Add Offline Modal */}
      <Modal open={showAddOffline} onClose={() => setShowOffline(false)} title={t('accounts.modal_offline_title')}>
        {/* autocomplete off + a stable distinct name mirrors the wizard's
            name field: without it Chromium (WebView2) pops the «сохранённые
            сведения» autofill dropdown over the nickname box (wave 26). */}
        <Input label={t('accounts.username_label')} id="offline-name" name="offline-username-new" type="text" autoComplete="off" value={offlineName} onChange={(e) => setOfflineName(e.target.value)} autoFocus />
        {offlineNameError && (
          <div style={{ marginTop: 6, fontSize: 'var(--font-size-xs)', color: 'var(--color-error, #ff6b6b)' }}>
            {offlineNameError}
          </div>
        )}
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setShowOffline(false)}>{t('common.cancel')}</Button>
          <Button onClick={handleAddOffline} loading={addingOffline} disabled={!offlineName.trim() || !!offlineNameError || addingOffline}><Plus size={14} /> {t('common.add')}</Button>
        </div>
      </Modal>

      {/* Add Ely.by Modal */}
      <Modal open={showAddElyby} onClose={() => setShowElyby(false)} title={t('accounts.modal_elyby_title')}>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
          <Input label={t('accounts.elyby_email_label')} id="elyby-user" type="text" value={elybyUser} onChange={(e) => setElybyUser(e.target.value)} autoFocus />
          <Input label={t('accounts.password_label')} id="elyby-pass" type="password" value={elybyPass} onChange={(e) => setElybyPass(e.target.value)} />
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setShowElyby(false)}>{t('common.cancel')}</Button>
          <Button onClick={handleAddElyby} loading={addingElyby} disabled={!elybyUser.trim() || !elybyPass.trim() || addingElyby}><Plus size={14} /> {t('common.add')}</Button>
        </div>
      </Modal>

      {/* Add Microsoft Modal — reuses the login card from the main-menu "ВОЙТИ"
          page, but without the card's own glass: the dialog sheet is the only
          visible surface (owner: the inner panel highlighted on hover while the
          sheet behind it stayed flat). `bare` still keeps the page undimmed.
          onSuccess closes the modal and reloads so any number of Microsoft
          accounts can be added one after another. */}
      <Modal open={showMicrosoftLogin} onClose={() => setShowMicrosoftLogin(false)} maxWidth={460} bare>
        <MicrosoftLoginCard embedded onSuccess={() => { setShowMicrosoftLogin(false); loadAccounts(true); }} />
      </Modal>

      {/* Skin Variant Modal */}
      <Modal open={!!skinModalAccount} onClose={() => setSkinModalAccount(null)} title={t('accounts.skin_variant_title')}>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
          <CustomSelect<'classic' | 'slim'>
            value={skinVariant}
            options={[
              { value: 'classic', label: t('accounts.skin_variant_classic') },
              { value: 'slim', label: t('accounts.skin_variant_slim') },
            ]}
            onChange={setSkinVariant}
          />
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setSkinModalAccount(null)}>{t('common.cancel')}</Button>
          <Button onClick={confirmSkinChange}>{t('common.confirm')}</Button>
        </div>
      </Modal>

      {/* Ely.by skin help — no public skin-upload API, point the user at the
          website. The launcher still SHOWS the actual Ely.by skin in 3D. */}
      <ConfirmDialog
        open={!!removeTarget}
        title={t('accounts.remove_title')}
        description={t('accounts.remove_confirm', { name: removeTarget?.name ?? '' })}
        confirmLabel={t('accounts.remove_btn')}
        tone="primary"
        busy={removing}
        onCancel={() => { if (!removing) setRemoveTarget(null); }}
        onConfirm={() => { if (removeTarget) handleRemoveAccount(removeTarget); }}
      />

      <Modal
        open={showElybySkinHelp}
        onClose={() => setShowElybySkinHelp(false)}
        title={t('accounts.elyby_skin_help_title')}
        maxWidth={420}
      >
        <p style={{ margin: 0, fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)', lineHeight: 1.5 }}>
          {t('accounts.elyby_skin_help_body')}
        </p>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setShowElybySkinHelp(false)}>{t('common.close')}</Button>
          <Button onClick={() => { openUrl(ELYBY_SKINS_URL).catch(() => {}); }}>
            {t('accounts.elyby_skin_help_open')}
          </Button>
        </div>
      </Modal>
    </div>
  );
}
