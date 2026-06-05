import { useEffect, useState } from 'react';
import { Plus, Play, Trash2, Package, X } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useInstanceStore } from '../stores/instanceStore';
import { t, formatPlayTime } from '../lib/i18n';
import { LoadingSpinner } from '../components/ui/LoadingSpinner';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';

interface VersionEntry {
  id: string;
  type: string;
  url: string;
  releaseTime: string;
}

interface VersionManifest {
  latest: { release: string; snapshot: string };
  versions: VersionEntry[];
}

export function Instances() {
  const instances = useInstanceStore((s) => s.instances);
  const error = useInstanceStore((s) => s.error);
  const isLaunching = useInstanceStore((s) => s.isLaunching);
  const launchStatus = useInstanceStore((s) => s.launchStatus);
  const loadInstances = useInstanceStore((s) => s.loadInstances);
  const createInstance = useInstanceStore((s) => s.createInstance);
  const deleteInstance = useInstanceStore((s) => s.deleteInstance);
  const selectInstance = useInstanceStore((s) => s.selectInstance);
  const launchGame = useInstanceStore((s) => s.launchGame);
  const installVersion = useInstanceStore((s) => s.installVersion);
  const clearError = useInstanceStore((s) => s.clearError);

  const [showCreateModal, setShowCreateModal] = useState(false);
  const [newName, setNewName] = useState('');
  const [selectedVersion, setSelectedVersion] = useState('');
  const [versions, setVersions] = useState<VersionEntry[]>([]);
  const [versionFilter, setVersionFilter] = useState<'release' | 'snapshot' | 'all'>('release');
  const [isCreating, setIsCreating] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; name: string } | null>(null);

  useEffect(() => {
    loadInstances();
  }, []);

  const fetchVersions = async () => {
    try {
      const manifest = await invoke<VersionManifest>('cmd_get_versions');
      setVersions(manifest.versions);
      setSelectedVersion(manifest.latest.release);
    } catch (e) {
      console.error('Failed to fetch versions:', e);
    }
  };

  const handleOpenCreate = () => {
    setShowCreateModal(true);
    setNewName('');
    fetchVersions();
  };

  const handleCreate = async () => {
    if (!newName.trim() || !selectedVersion) return;
    setIsCreating(true);

    try {
      // Find version URL and install it
      const version = versions.find((v) => v.id === selectedVersion);
      if (version) {
        await installVersion(version.url);
      }
      await createInstance(newName.trim(), selectedVersion);
      setShowCreateModal(false);
      setNewName('');
    } catch (e) {
      console.error('Failed to create instance:', e);
    }
    setIsCreating(false);
  };

  const handleContextMenu = (e: React.MouseEvent, name: string) => {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY, name });
  };

  const filteredVersions = versions.filter((v) => {
    if (versionFilter === 'release') return v.type === 'release';
    if (versionFilter === 'snapshot') return v.type === 'snapshot';
    return true;
  });

  const getLoaderClass = (loader: string) => {
    return `instance-card__tag--${loader.toLowerCase()}`;
  };

  const formatInstancePlayTime = (seconds: number) => {
    if (seconds === 0) return t('instances.never_played');
    return formatPlayTime(seconds);
  };

  return (
    <div className="page animate-fade-in" onClick={() => setContextMenu(null)}>
      <div className="page__header" style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
        <div>
          <h1 className="page__title">{t('instances.page_title')}</h1>
          <p className="page__subtitle">{t('instances.count', { n: instances.length.toString() })}</p>
        </div>
        <Button onClick={handleOpenCreate} id="create-instance-btn">
          <Plus size={16} />
          {t('instances.new_btn')}
        </Button>
      </div>

      {error && (
        <div className="toast toast--error" style={{ marginBottom: 'var(--space-lg)' }}>
          <span>{error}</span>
          <Button variant="ghost" size="sm" onClick={clearError} style={{ marginLeft: 'auto' }}>
            <X size={14} />
          </Button>
        </div>
      )}

      {launchStatus && (
        <div className="glass-card" style={{ marginBottom: 'var(--space-lg)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)' }}>
          <LoadingSpinner />
          <span>{launchStatus}</span>
        </div>
      )}

      {instances.length === 0 ? (
        <div className="empty-state">
          <div className="empty-state__icon">
            <Package size={64} />
          </div>
          <h3 className="empty-state__title">{t('instances.empty_title')}</h3>
          <p className="empty-state__desc">
            {t('instances.empty_desc')}
          </p>
          <Button onClick={handleOpenCreate}>
            {t('instances.empty_cta')}
          </Button>
        </div>
      ) : (
        <div className="instance-grid">
          {instances.map((instance) => (
            <div
              key={instance.name}
              className="instance-card"
              onContextMenu={(e) => handleContextMenu(e, instance.name)}
              onClick={() => selectInstance(instance.name)}
            >
              <div className="instance-card__banner" />
              <div className="instance-card__icon">
                <Package size={24} />
              </div>
              <div className="instance-card__body">
                <div className="instance-card__name">{instance.name}</div>
                <div className="instance-card__meta">
                  <span className={`instance-card__tag ${getLoaderClass(instance.loader)}`}>
                    {instance.loader}
                  </span>
                  <span>{instance.mc_version}</span>
                </div>
                <p style={{
                  fontSize: 'var(--font-size-xs)',
                  color: 'var(--text-tertiary)',
                  marginTop: 'var(--space-sm)',
                }}>
                  {formatInstancePlayTime(instance.play_time_seconds)}
                </p>
              </div>
              <div className="instance-card__actions">
                <Button
                  size="sm"
                  onClick={(e) => {
                    e.stopPropagation();
                    launchGame(instance.name);
                  }}
                  disabled={isLaunching}
                >
                  <Play size={14} fill="currentColor" /> {t('instances.play')}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={(e) => {
                    e.stopPropagation();
                    deleteInstance(instance.name);
                  }}
                >
                  <Trash2 size={14} />
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Context Menu */}
      {contextMenu && (
        <div
          style={{
            position: 'fixed',
            left: contextMenu.x,
            top: contextMenu.y,
            background: 'var(--bg-elevated)',
            border: '1px solid var(--surface-border)',
            borderRadius: 'var(--radius-md)',
            padding: 'var(--space-xs)',
            zIndex: 2000,
            minWidth: 160,
            boxShadow: 'var(--shadow-lg)',
          }}
        >
          <Button variant="ghost" style={{ width: '100%', justifyContent: 'flex-start' }}
            onClick={() => { launchGame(contextMenu.name); setContextMenu(null); }}>
              <Play size={14} fill="currentColor" /> {t('instances.play')}
            </Button>
            <Button variant="ghost" style={{ width: '100%', justifyContent: 'flex-start' }}
              onClick={() => { deleteInstance(contextMenu.name); setContextMenu(null); }}>
              <Trash2 size={14} /> {t('common.delete')}
          </Button>
        </div>
      )}

      {/* Create Instance Modal */}
      <Modal
        open={showCreateModal}
        onClose={() => setShowCreateModal(false)}
        title="New Instance"
        footer={
          <>
            <Button variant="ghost" onClick={() => setShowCreateModal(false)}>
              {t('common.cancel')}
            </Button>
            <Button
              onClick={handleCreate}
              disabled={!newName.trim() || !selectedVersion || isCreating}
              loading={isCreating}
              id="create-instance-confirm"
            >
              {t('common.save')}
            </Button>
          </>
        }
      >
        <div className="input-group" style={{ marginBottom: 'var(--space-xl)' }}>
          <label className="input-group__label">{t('create_instance.name_label')}</label>
          <input
            className="input"
            type="text"
            placeholder={t('create_instance.name_placeholder')}
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            autoFocus
            id="instance-name-input"
          />
        </div>

        <div className="input-group" style={{ marginBottom: 'var(--space-lg)' }}>
          <label className="input-group__label">{t('create_instance.version_label')}</label>
          <div style={{ display: 'flex', gap: 'var(--space-xs)', marginBottom: 'var(--space-sm)' }}>
            {(['release', 'snapshot', 'all'] as const).map((filter) => (
              <Button
                key={filter}
                size="sm"
                variant={versionFilter === filter ? 'primary' : 'ghost'}
                onClick={() => setVersionFilter(filter)}
              >
                {filter.charAt(0).toUpperCase() + filter.slice(1)}
              </Button>
            ))}
          </div>
          <div className="version-list">
            {filteredVersions.slice(0, 50).map((v) => (
              <div
                key={v.id}
                className={`version-item ${selectedVersion === v.id ? 'version-item--selected' : ''}`}
                onClick={() => setSelectedVersion(v.id)}
              >
                <span className="version-item__name">{v.id}</span>
                <span className="version-item__type">{v.type}</span>
              </div>
            ))}
          </div>
        </div>
      </Modal>
    </div>
  );
}


