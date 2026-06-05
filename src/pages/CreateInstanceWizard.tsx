import { useEffect, useState } from 'react';
import { t } from '../lib/i18n';
import { invoke } from '@tauri-apps/api/core';
import { Search } from 'lucide-react';
import { Modal } from '../components/ui/Modal';
import { Button } from '../components/ui/Button';
import { Input } from '../components/ui/Input';
import { Skeleton } from '../components/ui/Skeleton';
import { useInstanceStore } from '../stores/instanceStore';
import { addToast } from '../components/ui/Toast';

interface VersionEntry {
  id: string;
  type: string;
  url: string;
}

interface LoaderVersion {
  version: string;
  stable: boolean;
}

interface CreateWizardProps {
  open: boolean;
  onClose: () => void;
}

type LoaderType = 'Vanilla' | 'Fabric' | 'Quilt' | 'Forge' | 'NeoForge';

export function CreateInstanceWizard({ open, onClose }: CreateWizardProps) {
  const { createInstance, installVersion, saveInstance } = useInstanceStore();

  const [versions, setVersions] = useState<VersionEntry[]>([]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [selectedVersion, setSelectedVersion] = useState('');
  const [versionFilter, setVersionFilter] = useState<'release' | 'snapshot' | 'all'>('release');
  const [searchQuery, setSearchQuery] = useState('');

  const [loaderType, setLoaderType] = useState<LoaderType>('Vanilla');
  const [fabricVersions, setFabricVersions] = useState<LoaderVersion[]>([]);
  const [quiltVersions, setQuiltVersions] = useState<LoaderVersion[]>([]);
  const [forgeVersions, setForgeVersions] = useState<LoaderVersion[]>([]);
  const [neoforgeVersions, setNeoForgeVersions] = useState<LoaderVersion[]>([]);
  const [selectedLoaderVersion, setSelectedLoaderVersion] = useState('');

  const [instanceName, setInstanceName] = useState('');
  const [isCreating, setIsCreating] = useState(false);

  useEffect(() => {
    if (!open) return;
    reset();
    fetchVersions();
  }, [open]);

  const reset = () => {
    setSelectedVersion('');
    setLoaderType('Vanilla');
    setSelectedLoaderVersion('');
    setInstanceName('');
    setIsCreating(false);
    setSearchQuery('');
  };

  const fetchVersions = async () => {
    setVersionsLoading(true);
    try {
      const manifest = await invoke<{ versions: VersionEntry[] }>('cmd_get_versions');
      setVersions(manifest.versions);
      if (manifest.versions.length > 0) {
        setSelectedVersion(manifest.versions.find((v) => v.type === 'release')?.id || manifest.versions[0].id);
      }
    } catch (e) {
      console.error('Failed to fetch versions:', e);
    }
    setVersionsLoading(false);
  };

  const fetchLoaderVersions = async (loader: string) => {
    try {
      let v: LoaderVersion[] = [];
      if (loader === 'Fabric') v = await invoke<LoaderVersion[]>('cmd_get_fabric_versions');
      else if (loader === 'Quilt') v = await invoke<LoaderVersion[]>('cmd_get_quilt_versions');
      else if (loader === 'Forge') v = await invoke<LoaderVersion[]>('cmd_get_forge_versions', { mcVersion: selectedVersion });
      else if (loader === 'NeoForge') v = await invoke<LoaderVersion[]>('cmd_get_neoforge_versions', { mcVersion: selectedVersion });

      if (loader === 'Fabric') setFabricVersions(v);
      else if (loader === 'Quilt') setQuiltVersions(v);
      else if (loader === 'Forge') setForgeVersions(v);
      else if (loader === 'NeoForge') setNeoForgeVersions(v);

      if (v.length > 0) setSelectedLoaderVersion(v[0].version);
    } catch (e) {
      console.error('Failed to fetch loader versions:', e);
    }
  };

  const handleLoaderChange = (loader: LoaderType) => {
    setLoaderType(loader);
    setSelectedLoaderVersion('');
    if (loader !== 'Vanilla') fetchLoaderVersions(loader);
  };

  const filteredVersions = versions.filter((v) => {
    if (versionFilter === 'release' && v.type !== 'release') return false;
    if (versionFilter === 'snapshot' && v.type !== 'snapshot') return false;
    if (searchQuery && !v.id.toLowerCase().includes(searchQuery.toLowerCase())) return false;
    return true;
  });

  const currentLoaderVersions = loaderType === 'Fabric' ? fabricVersions
    : loaderType === 'Quilt' ? quiltVersions
    : loaderType === 'Forge' ? forgeVersions
    : loaderType === 'NeoForge' ? neoforgeVersions
    : [];

  const canCreate = instanceName.trim() !== '' && selectedVersion !== '' && !isCreating
    && (loaderType === 'Vanilla' || selectedLoaderVersion !== '');

  const handleCreate = async () => {
    if (!canCreate) return;
    setIsCreating(true);

    try {
      const versionEntry = versions.find((v) => v.id === selectedVersion);
      if (!versionEntry) throw new Error('Version not found');

      addToast(t('create_instance.downloading'), 'info');
      await createInstance(instanceName.trim(), selectedVersion);
      await installVersion(versionEntry.url, instanceName.trim());

      if (loaderType !== 'Vanilla' && selectedLoaderVersion) {
        const instances = useInstanceStore.getState().instances;
        const created = instances.find((i) => i.name === instanceName.trim());
        if (created) {
          await saveInstance({ ...created, loader: loaderType, loader_version: selectedLoaderVersion });
        }

        const cmd = loaderType === 'Fabric' ? 'cmd_install_fabric'
          : loaderType === 'Quilt' ? 'cmd_install_quilt'
          : loaderType === 'Forge' ? 'cmd_install_forge'
          : 'cmd_install_neoforge';
        try {
          await invoke(cmd, { mcVersion: selectedVersion, loaderVersion: selectedLoaderVersion, instanceName: instanceName.trim() });
        } catch (e) {
          addToast(t('create_instance.loader_failed', { loader: loaderType }), 'warning');
        }
      }

      addToast(t('create_instance.created', { name: instanceName }), 'success');
      onClose();
    } catch (e: any) {
      addToast(t('create_instance.failed', { error: e.toString() }), 'error');
    }
    setIsCreating(false);
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t('create_instance.title')}
      maxWidth={700}
      footer={
        <div style={{ display: 'flex', justifyContent: 'space-between', width: '100%' }}>
          <Button variant="ghost" onClick={onClose}>{t('common.cancel')}</Button>
          <Button onClick={handleCreate} loading={isCreating} disabled={!canCreate}>
            {t('create_instance.title')}
          </Button>
        </div>
      }
    >
      {/* Instance Name */}
      <div style={{ marginBottom: 'var(--space-xl)' }}>
        <Input
          label={t('create_instance.name_label')}
          id="wizard-name"
          type="text"
          placeholder={t('create_instance.name_placeholder')}
          value={instanceName}
          onChange={(e) => setInstanceName(e.target.value)}
          autoFocus
        />
      </div>

      {/* MC Version + Loader in two columns */}
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 'var(--space-xl)' }}>
        {/* Left: Version Selection */}
        <div>
          <label className="input-group__label" style={{ display: 'block', marginBottom: 'var(--space-sm)', fontWeight: 600, fontSize: 'var(--font-size-sm)' }}>
            {t('create_instance.version_label')}
          </label>
          <div style={{ position: 'relative', marginBottom: 'var(--space-sm)' }}>
            <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--text-tertiary)' }} />
            <input
              className="input"
              type="text"
              placeholder={t('create_instance.search_placeholder')}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              style={{ paddingLeft: 32, fontSize: 'var(--font-size-sm)' }}
            />
          </div>
          <div style={{ display: 'flex', gap: 4, marginBottom: 'var(--space-sm)' }}>
            {(['release', 'snapshot', 'all'] as const).map((f) => (
              <Button key={f} size="sm" variant={versionFilter === f ? 'primary' : 'ghost'} onClick={() => setVersionFilter(f)} style={{ fontSize: '11px', padding: '4px 8px' }}>
                {f === 'all' ? t('create_instance.filter_all') : f === 'release' ? t('create_instance.filter_release') : t('create_instance.filter_snapshot')}
              </Button>
            ))}
          </div>
          <div style={{ maxHeight: 200, overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
            {versionsLoading ? (
              Array.from({ length: 6 }).map((_, i) => <Skeleton key={i} height={32} style={{ marginBottom: 2 }} />)
            ) : (
              filteredVersions.slice(0, 30).map((v) => (
                <div
                  key={v.id}
                  onClick={() => setSelectedVersion(v.id)}
                  style={{
                    padding: '6px 10px', cursor: 'pointer', fontSize: 'var(--font-size-sm)',
                    background: selectedVersion === v.id ? 'var(--primary)' : 'transparent',
                    color: selectedVersion === v.id ? 'white' : 'var(--text-primary)',
                    borderRadius: 'var(--radius-sm)',
                    display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                  }}
                >
                  <span>{v.id}</span>
                  <span style={{ fontSize: 10, opacity: 0.6 }}>{v.type}</span>
                </div>
              ))
            )}
          </div>
        </div>

        {/* Right: Loader Selection */}
        <div>
          <label className="input-group__label" style={{ display: 'block', marginBottom: 'var(--space-sm)', fontWeight: 600, fontSize: 'var(--font-size-sm)' }}>
            {t('create_instance.loader_label')}
          </label>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 6, marginBottom: 'var(--space-sm)' }}>
            {(['Vanilla', 'Fabric', 'Quilt', 'Forge', 'NeoForge'] as const).map((l) => (
              <Button
                key={l}
                size="sm"
                variant={loaderType === l ? 'primary' : 'ghost'}
                onClick={() => handleLoaderChange(l)}
                style={{ fontSize: '11px' }}
              >
                {l === 'Vanilla' ? t('create_instance.loader_vanilla') : l === 'Fabric' ? t('create_instance.loader_fabric') : l === 'Quilt' ? t('create_instance.loader_quilt') : l === 'Forge' ? t('create_instance.loader_forge') : t('create_instance.loader_neoforge')}
              </Button>
            ))}
          </div>

          {loaderType !== 'Vanilla' && (
            <div style={{ maxHeight: 168, overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
              {currentLoaderVersions.length === 0 ? (
                <Skeleton height={32} />
              ) : (
                currentLoaderVersions.slice(0, 20).map((lv) => (
                  <div
                    key={lv.version}
                    onClick={() => setSelectedLoaderVersion(lv.version)}
                    style={{
                      padding: '6px 10px', cursor: 'pointer', fontSize: 'var(--font-size-sm)',
                      background: selectedLoaderVersion === lv.version ? 'var(--primary)' : 'transparent',
                      color: selectedLoaderVersion === lv.version ? 'white' : 'var(--text-primary)',
                      borderRadius: 'var(--radius-sm)',
                      display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                    }}
                  >
                    <span>{lv.version}</span>
                    {lv.stable && <span style={{ fontSize: 10, opacity: 0.6 }}>{t('create_instance.stable_badge')}</span>}
                  </div>
                ))
              )}
            </div>
          )}
          {loaderType === 'Vanilla' && (
            <p style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)', marginTop: 'var(--space-sm)' }}>
              {t('create_instance.vanilla_hint')}
            </p>
          )}
        </div>
      </div>
    </Modal>
  );
}
