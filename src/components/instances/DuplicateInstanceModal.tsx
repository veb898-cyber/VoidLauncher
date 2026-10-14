import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Copy } from 'lucide-react';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { Input } from '../ui/Input';
import { Toggle } from '../ui/Toggle';
import { addToast } from '../ui/Toast';
import { t } from '../../lib/i18n';

interface Props {
  instanceName: string;
  onClose: () => void;
  /** Called after a successful duplication so the caller can reload the list. */
  onDuplicated: () => void;
}

/**
 * Duplicates an instance. Metadata, mods, configs, resource packs and
 * shaders always travel to the copy; the user can additionally leave out
 * saves, screenshots and logs (the source's `.backups` history never moves).
 */
export function DuplicateInstanceModal({ instanceName, onClose, onDuplicated }: Props) {
  const suggested = `${instanceName} ${t('worlds.copy_suffix')}`.trim().slice(0, 64);
  const [newName, setNewName] = useState(suggested);
  const [saves, setSaves] = useState(true);
  const [screenshots, setScreenshots] = useState(true);
  const [logs, setLogs] = useState(false);
  const [busy, setBusy] = useState(false);

  const name = newName.trim();
  const nameValid = name.length >= 3 && name.length <= 64 && !name.startsWith('.');

  const doDuplicate = async () => {
    if (!nameValid || busy) return;
    setBusy(true);
    try {
      await invoke('cmd_duplicate_instance', {
        name: instanceName,
        newName: name,
        excludeSaves: !saves,
        excludeScreenshots: !screenshots,
        excludeLogs: !logs,
      });
      addToast(t('instances.duplicate_toast', { name }), 'success');
      onDuplicated();
      onClose();
    } catch (e: any) {
      addToast(t('instances.duplicate_error', { error: e.toString() }), 'error');
    }
    setBusy(false);
  };

  const toggleRow = (checked: boolean, onChange: (v: boolean) => void, label: string) => (
    <label
      style={{
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'space-between',
        gap: 'var(--space-sm)',
        padding: '8px 10px',
        borderRadius: 'var(--radius-sm)',
        border: '1px solid var(--surface-border)',
        cursor: 'pointer',
        fontSize: 'var(--font-size-sm)',
        userSelect: 'none',
        transition: 'border-color 160ms ease, background 160ms ease',
      }}
    >
      <span>{label}</span>
      <Toggle as="span" checked={checked} onChange={onChange} />
    </label>
  );

  return (
    <Modal
      open
      onClose={onClose}
      title={t('instances.duplicate_title', { name: instanceName })}
      maxWidth={480}
    >
      <div style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'flex-start' }}>
        <Copy size={16} style={{ marginTop: 22, color: 'var(--text-tertiary)', flexShrink: 0 }} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <Input
            label={t('instances.duplicate_name_label')}
            id="instance-duplicate-name"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            autoFocus
          />
        </div>
      </div>

      <div style={{ marginTop: 'var(--space-md)', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
        {t('instances.duplicate_always')}
      </div>

      <div style={{ display: 'flex', flexDirection: 'column', gap: 6, marginTop: 'var(--space-sm)' }}>
        {toggleRow(saves, setSaves, t('instances.duplicate_saves'))}
        {toggleRow(screenshots, setScreenshots, t('instances.duplicate_screenshots'))}
        {toggleRow(logs, setLogs, t('instances.duplicate_logs'))}
      </div>

      <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
        <Button variant="ghost" onClick={onClose}>{t('common.cancel')}</Button>
        <Button disabled={!nameValid || busy} onClick={doDuplicate}>
          {t('instances.duplicate_confirm')}
        </Button>
      </div>
    </Modal>
  );
}
