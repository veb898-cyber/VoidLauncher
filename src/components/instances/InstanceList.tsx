import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Plus, Package, Play, Settings, FolderOpen, Trash2, ShieldCheck, Download } from 'lucide-react';
import { useInstanceStore } from '../../stores/instanceStore';
import { useBrowserGuardStore } from '../../stores/browserGuardStore';
import { t } from '../../lib/i18n';
import { Button } from '../ui/Button';
import { ConfirmDialog } from '../ui/ConfirmDialog';
import { EmptyState } from '../ui/EmptyState';
import { ProgressBar } from '../ui/ProgressBar';
import { addToast } from '../ui/Toast';
import { InstanceEditor } from './InstanceEditor';
import { VerifyRepairModal } from './VerifyRepairModal';
import { useEventStore } from '../../hooks/useGameEvents';

interface InstanceListProps {
  onCreateClick: () => void;
}

interface ContextMenuState {
  x: number;
  y: number;
  name: string;
}

export function InstanceList({ onCreateClick }: InstanceListProps) {
  const instances = useInstanceStore((s) => s.instances);
  const selectedInstance = useInstanceStore((s) => s.selectedInstance);
  const selectInstance = useInstanceStore((s) => s.selectInstance);
  const launchGame = useInstanceStore((s) => s.launchGame);
  const deleteInstance = useInstanceStore((s) => s.deleteInstance);
  const isLaunching = useInstanceStore((s) => s.isLaunching);
  const installProgress = useEventStore((s) => s.installProgress);

  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [deleteConfirmName, setDeleteConfirmName] = useState<string | null>(null);
  const [editorName, setEditorName] = useState<string | null>(null);
  const [verifyName, setVerifyName] = useState<string | null>(null);
  const editorInstance = editorName ? instances.find((i) => i.name === editorName) ?? null : null;

  const installingName = installProgress?.instance_id || null;

  // Close the context menu on any click outside of it.
  useEffect(() => {
    if (!contextMenu) return;
    const handler = () => setContextMenu(null);
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [contextMenu]);

  const openFolder = async (name: string, subfolder?: string) => {
    try {
      await invoke('cmd_open_instance_folder', { instanceName: name, subfolder: subfolder ?? null });
    } catch (e) {
      console.error('Failed to open instance folder:', e);
    }
  };

  // Export the instance as a Prism-compatible .zip the user can import
  // into Prism Launcher or share. The save dialog suggests `<name>.zip`.
  const handleExport = async (name: string) => {
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const selected = await save({
        defaultPath: `${name}.zip`,
        filters: [{ name: 'Zip archive', extensions: ['zip'] }],
      });
      if (!selected) return;
      const path = selected.toLowerCase().endsWith('.zip') ? selected : `${selected}.zip`;
      await invoke('cmd_export_instance', { name, outputPath: path });
      addToast(t('instances.export_success', { name }), 'success');
    } catch (e: any) {
      addToast(t('instances.export_error', { error: e.toString() }), 'error');
    }
  };

  return (
    <div
      className="instance-list"
      style={{
        width: 260,
        borderRight: '1px solid var(--surface-border)',
        display: 'flex',
        flexDirection: 'column',
        flexShrink: 0,
      }}
    >
      {/* Header */}
      <div
        style={{
          padding: 'var(--space-lg)',
          borderBottom: '1px solid var(--surface-border)',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
        }}
      >
        <span style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)', textTransform: 'uppercase', letterSpacing: '0.5px', color: 'var(--text-secondary)' }}>
          {t('instances.page_title')}
        </span>
        <Button size="sm" onClick={onCreateClick}>
          <Plus size={14} />
        </Button>
      </div>

      {/* List */}
      <div style={{ flex: 1, overflowY: 'auto', padding: 'var(--space-sm)' }}>
        {instances.length === 0 ? (
          <EmptyState compact icon={<Package size={24} />} title={t('instances.empty_title')} />
        ) : (
          instances.map((instance) => {
            const isInstalling = installingName === instance.name;
            const isSelected = selectedInstance === instance.name;

            return (
              <div
                key={instance.name}
                onClick={() => useBrowserGuardStore.getState().askLeave(() => selectInstance(instance.name))}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setContextMenu({ x: e.clientX, y: e.clientY, name: instance.name });
                }}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 'var(--space-md)',
                  padding: 'var(--space-md)',
                  borderRadius: 'var(--radius-md)',
                  background: isSelected ? 'var(--accent-dim)' : 'transparent',
                  cursor: 'pointer',
                  transition: 'background var(--transition-fast)',
                  marginBottom: 2,
                  opacity: isLaunching && !isSelected ? 0.6 : 1,
                }}
                onMouseEnter={(e) => {
                  if (!isSelected) e.currentTarget.style.background = 'var(--surface-glass-hover)';
                }}
                onMouseLeave={(e) => {
                  if (!isSelected) e.currentTarget.style.background = 'transparent';
                }}
              >
                <div
                  style={{
                    width: 36,
                    height: 36,
                    borderRadius: 'var(--radius-md)',
                    background: 'var(--surface-glass)',
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    flexShrink: 0,
                    fontSize: 'var(--font-size-lg)',
                  }}
                >
                  <Package size={18} color="var(--accent)" />
                </div>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div
                    style={{
                      fontSize: 'var(--font-size-sm)',
                      fontWeight: 500,
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                    }}
                  >
                    {instance.name}
                  </div>
                  <div
                    style={{
                      fontSize: 'var(--font-size-xs)',
                      color: 'var(--text-tertiary)',
                    }}
                  >
                    {instance.loader !== 'Vanilla' ? `${instance.loader} ` : ''}
                    {instance.mc_version}
                  </div>
                  {isInstalling && (
                    <div style={{ marginTop: 4 }}>
                      <ProgressBar percent={installProgress?.percent || 0} />
                    </div>
                  )}
                </div>
              </div>
            );
          })
        )}
      </div>

      {/* Context Menu */}
      {contextMenu && (
        <div
          className="ctx-menu"
          style={{
            left: Math.min(contextMenu.x, window.innerWidth - 200),
            top: Math.min(contextMenu.y, window.innerHeight - 250),
          }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <ContextMenuItem icon={<Play size={14} fill="currentColor" />} label={t('instances.play')} disabled={isLaunching}
            onClick={() => { launchGame(contextMenu.name); setContextMenu(null); }} />
          <ContextMenuItem icon={<Settings size={14} />} label={t('instances.ctx_settings')}
            onClick={() => { setEditorName(contextMenu.name); setContextMenu(null); }} />
          <ContextMenuItem icon={<FolderOpen size={14} />} label={t('instances.ctx_open_folder')}
            onClick={() => { openFolder(contextMenu.name); setContextMenu(null); }} />
          <ContextMenuItem icon={<ShieldCheck size={14} />} label={t('instances.ctx_verify')}
            onClick={() => { setVerifyName(contextMenu.name); setContextMenu(null); }} />
          <ContextMenuItem icon={<Download size={14} />} label={t('instances.ctx_export')}
            onClick={() => { handleExport(contextMenu.name); setContextMenu(null); }} />
          <ContextMenuItem icon={<Trash2 size={14} />} label={t('common.delete')} danger
            onClick={() => { setDeleteConfirmName(contextMenu.name); setContextMenu(null); }} />
        </div>
      )}

      {/* Verify / Repair modal */}
      {verifyName && (
        <VerifyRepairModal
          open
          instanceName={verifyName}
          onClose={() => setVerifyName(null)}
        />
      )}

      {/* Instance Settings Editor (opened from the card context menu) */}
      {editorInstance && (
        <InstanceEditor
          open
          instance={editorInstance}
          onClose={() => setEditorName(null)}
          onSaved={() => useInstanceStore.getState().loadInstances()}
        />
      )}

      {/* Delete Confirmation — shared confirm modal (lg: scrim + glass sheet) */}
      <ConfirmDialog
        open={!!deleteConfirmName}
        title={t('instance_detail.confirm_delete_title')}
        description={t('instance_detail.confirm_delete_text', { name: deleteConfirmName ?? '' })}
        onConfirm={() => {
          if (deleteConfirmName) deleteInstance(deleteConfirmName);
          setDeleteConfirmName(null);
        }}
        onCancel={() => setDeleteConfirmName(null)}
        confirmLabel={t('common.delete')}
      />
    </div>
  );
}

function ContextMenuItem({ icon, label, onClick, disabled, danger }: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      className={`ctx-menu__item${danger ? ' ctx-menu__item--danger' : ''}`}
      disabled={disabled}
      onClick={onClick}
    >
      {icon} {label}
    </button>
  );
}
