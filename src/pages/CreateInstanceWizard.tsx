import { useCallback, useEffect, useRef, useState } from 'react';
import { t } from '../lib/i18n';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { Search, FileArchive, Package, Plus, Upload } from 'lucide-react';
import { Modal } from '../components/ui/Modal';
import { Button } from '../components/ui/Button';
import { Input } from '../components/ui/Input';
import { Skeleton } from '../components/ui/Skeleton';
import { LoadingSpinner } from '../components/ui/LoadingSpinner';
import { useInstanceStore } from '../stores/instanceStore';
import { addToast } from '../components/ui/Toast';
import { useLogPlaque } from '../lib/uiLog';

interface VersionEntry {
  id: string;
  type: string;
  url: string;
}

interface LoaderVersion {
  version: string;
  stable: boolean;
}

interface LoaderVersionPage {
  versions: LoaderVersion[];
  total: number;
}

interface ModpackMetadata {
  format: string;
  name: string;
  mc_version: string | null;
  loader: string | null;
  loader_version: string | null;
  summary: string | null;
}

interface CreateWizardProps {
  open: boolean;
  onClose: () => void;
}

type LoaderType = 'Vanilla' | 'Fabric' | 'Quilt' | 'Forge' | 'NeoForge';

const LOADER_PAGE_SIZE = 20;
const SCROLL_LOAD_THRESHOLD_PX = 50;

type WizardMode = 'new' | 'import';

export function CreateInstanceWizard({ open, onClose }: CreateWizardProps) {
  const { createInstance, installVersion, loadInstances } = useInstanceStore();

  const [mode, setMode] = useState<WizardMode>('new');

  // New instance state
  const [versions, setVersions] = useState<VersionEntry[]>([]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [selectedVersion, setSelectedVersion] = useState('');
  const [versionFilter, setVersionFilter] = useState<'release' | 'snapshot' | 'all'>('release');
  const [searchQuery, setSearchQuery] = useState('');

  const [loaderType, setLoaderType] = useState<LoaderType>('Vanilla');
  const [loaderVersions, setLoaderVersions] = useState<Record<LoaderType, LoaderVersion[]>>({
    Vanilla: [], Fabric: [], Quilt: [], Forge: [], NeoForge: [],
  });
  const [loaderVersionTotals, setLoaderVersionTotals] = useState<Record<LoaderType, number | null>>({
    Vanilla: null, Fabric: null, Quilt: null, Forge: null, NeoForge: null,
  });
  const [loaderVersionsLoading, setLoaderVersionsLoading] = useState(false);
  const [loaderVersionsError, setLoaderVersionsError] = useState(false);
  const [selectedLoaderVersion, setSelectedLoaderVersion] = useState('');

  const [instanceName, setInstanceName] = useState('');
  const [isCreating, setIsCreating] = useState(false);

  // Import state
  const [importPath, setImportPath] = useState<string | null>(null);
  const [importMeta, setImportMeta] = useState<ModpackMetadata | null>(null);
  const [importLoading, setImportLoading] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);
  const [importProgress, setImportProgress] = useState<{ current: number; total: number; stage: string; message: string } | null>(null);

  useLogPlaque(importError, 'error', 'import');
  useLogPlaque(importProgress?.message ?? null, 'info', 'import');

  const loaderListRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    reset();
    fetchVersions();
  }, [open]);

  const reset = () => {
    setMode('new');
    setSelectedVersion('');
    setLoaderType('Vanilla');
    setSelectedLoaderVersion('');
    setInstanceName('');
    setIsCreating(false);
    setSearchQuery('');
    setLoaderVersionsLoading(false);
    setLoaderVersionsError(false);
    setImportPath(null);
    setImportMeta(null);
    setImportLoading(false);
    setImportError(null);
    setImportProgress(null);
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

  const fetchLoaderPage = useCallback(async (loader?: LoaderType) => {
    const target: LoaderType = loader ?? loaderType;
    if (target === 'Vanilla') return;
    if (loaderVersionsLoading) return;
    if (target !== 'Fabric' && target !== 'Quilt' && selectedVersion === '') {
      setLoaderVersions(prev => ({ ...prev, [target]: [] }));
      setLoaderVersionTotals(prev => ({ ...prev, [target]: 0 }));
      return;
    }
    const offset = loaderVersions[target]?.length ?? 0;
    const totalKnown = loaderVersionTotals[target];
    if (totalKnown !== null && offset >= totalKnown) return;
    setLoaderVersionsLoading(true);
    setLoaderVersionsError(false);
    try {
      let page: LoaderVersionPage;
      if (target === 'Fabric') {
        page = await invoke<LoaderVersionPage>('cmd_get_fabric_versions', { offset, limit: LOADER_PAGE_SIZE });
      } else if (target === 'Quilt') {
        page = await invoke<LoaderVersionPage>('cmd_get_quilt_versions', { offset, limit: LOADER_PAGE_SIZE });
      } else if (target === 'Forge') {
        page = await invoke<LoaderVersionPage>('cmd_get_forge_versions', { mcVersion: selectedVersion, offset, limit: LOADER_PAGE_SIZE });
      } else if (target === 'NeoForge') {
        page = await invoke<LoaderVersionPage>('cmd_get_neoforge_versions', { mcVersion: selectedVersion, offset, limit: LOADER_PAGE_SIZE });
      } else {
        page = { versions: [], total: 0 };
      }
      setLoaderVersions(prev => ({ ...prev, [target]: [...(prev[target] ?? []), ...page.versions] }));
      setLoaderVersionTotals(prev => ({ ...prev, [target]: page.total }));
      if (offset === 0 && page.versions.length > 0) {
        setSelectedLoaderVersion(page.versions[0].version);
      }
    } catch (e) {
      console.error('Failed to fetch loader versions:', e);
      setLoaderVersionsError(true);
    }
    setLoaderVersionsLoading(false);
  }, [loaderType, selectedVersion, loaderVersionsLoading, loaderVersions, loaderVersionTotals]);

  const handleLoaderChange = (loader: LoaderType) => {
    setLoaderType(loader);
    setSelectedLoaderVersion('');
    setLoaderVersionsError(false);
    if (loader === 'Vanilla') {
      setLoaderVersionsLoading(false);
      return;
    }
    setLoaderVersions(prev => ({ ...prev, [loader]: [] }));
    setLoaderVersionTotals(prev => ({ ...prev, [loader]: null }));
    fetchLoaderPage(loader);
  };

  const handleLoaderScroll = () => {
    const el = loaderListRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (distanceFromBottom > SCROLL_LOAD_THRESHOLD_PX) return;
    fetchLoaderPage();
  };

  const filteredVersions = versions.filter((v) => {
    if (versionFilter === 'release' && v.type !== 'release') return false;
    if (versionFilter === 'snapshot' && v.type !== 'snapshot') return false;
    if (searchQuery && !v.id.toLowerCase().includes(searchQuery.toLowerCase())) return false;
    return true;
  });

  const currentLoaderVersions = loaderVersions[loaderType] ?? [];
  const currentLoaderTotal = loaderVersionTotals[loaderType];
  const hasMoreLoaderVersions = currentLoaderTotal === null || currentLoaderVersions.length < currentLoaderTotal;

  // New instance validation
  const canCreate = mode === 'new' && instanceName.trim() !== '' && selectedVersion !== '' && !isCreating
    && (loaderType === 'Vanilla' || selectedLoaderVersion !== '');

  // Import validation
  const canImport = mode === 'import' && instanceName.trim() !== '' && importMeta !== null && !isCreating;

  const handleCreate = async () => {
    if (!canCreate) return;
    setIsCreating(true);
    try {
      const versionEntry = versions.find((v) => v.id === selectedVersion);
      if (!versionEntry) throw new Error('Version not found');
      addToast(t('create_instance.downloading'), 'info');
      const ldr = loaderType === 'Vanilla' ? undefined : loaderType;
      const ldrVer = loaderType === 'Vanilla' ? undefined : selectedLoaderVersion;
      await createInstance(instanceName.trim(), selectedVersion, ldr, ldrVer);
      await installVersion(versionEntry.url, instanceName.trim());
      if (loaderType !== 'Vanilla' && selectedLoaderVersion) {
        const cmd = loaderType === 'Fabric' ? 'cmd_install_fabric'
          : loaderType === 'Quilt' ? 'cmd_install_quilt'
          : loaderType === 'Forge' ? 'cmd_install_forge'
          : 'cmd_install_neoforge';
        await invoke(cmd, { mcVersion: selectedVersion, loaderVersion: selectedLoaderVersion, instanceName: instanceName.trim() });
      }
      addToast(t('create_instance.created', { name: instanceName }), 'success');
      onClose();
    } catch (e: any) {
      addToast(t('create_instance.failed', { error: e.toString() }), 'error');
    }
    setIsCreating(false);
  };

  const handlePickFile = async () => {
    try {
      const selected = await openFileDialog({
        multiple: false,
        filters: [
          { name: 'Modpack Archives', extensions: ['zip', 'mrpack'] },
        ],
      });
      if (!selected) return;
      setImportPath(selected);
      setImportLoading(true);
      setImportError(null);
      setImportMeta(null);
      try {
        const meta = await invoke<ModpackMetadata>('cmd_probe_modpack', { path: selected });
        setImportMeta(meta);
        setInstanceName(meta.name);
      } catch (e: any) {
        setImportError(e.toString());
      }
      setImportLoading(false);
    } catch (e) {
      console.error('File dialog error:', e);
    }
  };

  const handleImport = async () => {
    if (!canImport || !importPath) return;
    setIsCreating(true);
    setImportProgress({ current: 0, total: 1, stage: 'starting', message: t('create_instance.import_starting') });
    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await listen<{ current: number; total: number; stage: string; message: string }>('import-progress', (event) => {
        setImportProgress(event.payload);
      });
      await invoke('cmd_import_modpack', { path: importPath, instanceName: instanceName.trim() });
      addToast(t('create_instance.imported', { name: instanceName }), 'success');
      await loadInstances();
      onClose();
    } catch (e: any) {
      addToast(t('create_instance.failed', { error: e.toString() }), 'error');
    }
    if (unlisten) unlisten();
    setIsCreating(false);
    setImportProgress(null);
  };

  const formatLabel = (fmt: string) => {
    switch (fmt) {
      case 'Prism': return t('create_instance.format_prism');
      case 'Modrinth': return t('create_instance.format_modrinth');
      case 'CurseForge': return t('create_instance.format_curseforge');
      case 'ATLauncher': return t('create_instance.format_atlauncher');
      default: return fmt;
    }
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t('create_instance.title')}
      maxWidth={800}
      footer={
        <div style={{ display: 'flex', justifyContent: 'space-between', width: '100%' }}>
          <Button variant="ghost" onClick={onClose}>{t('common.cancel')}</Button>
          {mode === 'new' ? (
            <Button onClick={handleCreate} loading={isCreating} disabled={!canCreate}>
              {t('create_instance.title')}
            </Button>
          ) : (
            <Button onClick={handleImport} loading={isCreating} disabled={!canImport}>
              {t('create_instance.import_action')}
            </Button>
          )}
        </div>
      }
    >
      {/* Mode selector sidebar + content */}
      <div style={{ display: 'flex', gap: 'var(--space-xl)', minHeight: 360 }}>
        {/* Left sidebar — mode select */}
        <div style={{
          width: 140, flexShrink: 0,
          display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)',
          borderRight: '1px solid var(--surface-border)', paddingRight: 'var(--space-lg)',
        }}>
          <div
            onClick={() => setMode('new')}
            style={{
              padding: 'var(--space-md) var(--space-lg)',
              borderRadius: 'var(--radius-md)',
              cursor: 'pointer',
              background: mode === 'new' ? 'var(--primary-dim)' : 'transparent',
              color: mode === 'new' ? 'var(--primary)' : 'var(--text-secondary)',
              fontWeight: mode === 'new' ? 600 : 400,
              display: 'flex', alignItems: 'center', gap: 'var(--space-sm)',
              fontSize: 'var(--font-size-sm)',
            }}
          >
            <Plus size={16} /> {t('create_instance.mode_new')}
          </div>

          <div
            onClick={() => setMode('import')}
            style={{
              padding: 'var(--space-md) var(--space-lg)',
              borderRadius: 'var(--radius-md)',
              cursor: 'pointer',
              background: mode === 'import' ? 'var(--primary-dim)' : 'transparent',
              color: mode === 'import' ? 'var(--primary)' : 'var(--text-secondary)',
              fontWeight: mode === 'import' ? 600 : 400,
              display: 'flex', alignItems: 'center', gap: 'var(--space-sm)',
              fontSize: 'var(--font-size-sm)',
            }}
          >
            <Upload size={16} /> {t('create_instance.mode_import')}
          </div>
        </div>

        {/* Right content area */}
        <div style={{ flex: 1, minWidth: 0 }}>
          {/* Instance Name */}
          <div style={{ marginBottom: 'var(--space-lg)' }}>
            <Input
              label={t('create_instance.name_label')}
              id="wizard-name"
              type="text"
              placeholder={t('create_instance.name_placeholder')}
              value={instanceName}
              onChange={(e) => setInstanceName(e.target.value)}
              autoFocus={mode === 'new'}
            />
          </div>

          {mode === 'new' ? (
            <>
              {/* MC Version + Loader in two columns */}
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 'var(--space-xl)' }}>
                <div>
                  <label className="input-group__label" style={{ display: 'block', marginBottom: 'var(--space-sm)', fontWeight: 600, fontSize: 'var(--font-size-sm)' }}>
                    {t('create_instance.version_label')}
                  </label>
                  <div style={{ position: 'relative', marginBottom: 'var(--space-sm)' }}>
                    <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--text-tertiary)' }} />
                    <input className="input" type="text" placeholder={t('create_instance.search_placeholder')}
                      value={searchQuery} onChange={(e) => setSearchQuery(e.target.value)}
                      style={{ paddingLeft: 32, fontSize: 'var(--font-size-sm)' }} />
                  </div>
                  <div style={{ display: 'flex', gap: 4, marginBottom: 'var(--space-sm)' }}>
                    {(['release', 'snapshot', 'all'] as const).map((f) => (
                      <Button key={f} size="sm" variant={versionFilter === f ? 'primary' : 'ghost'}
                        onClick={() => setVersionFilter(f)} style={{ fontSize: '11px', padding: '4px 8px' }}>
                        {f === 'all' ? t('create_instance.filter_all') : f === 'release' ? t('create_instance.filter_release') : t('create_instance.filter_snapshot')}
                      </Button>
                    ))}
                  </div>
                  <div style={{ maxHeight: 200, overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
                    {versionsLoading ? (
                      Array.from({ length: 6 }).map((_, i) => <Skeleton key={i} height={32} style={{ marginBottom: 2 }} />)
                    ) : (
                      filteredVersions.map((v) => (
                        <div key={v.id} onClick={() => setSelectedVersion(v.id)}
                          style={{
                            padding: '6px 10px', cursor: 'pointer', fontSize: 'var(--font-size-sm)',
                            background: selectedVersion === v.id ? 'var(--primary)' : 'transparent',
                            color: selectedVersion === v.id ? 'white' : 'var(--text-primary)',
                            borderRadius: 'var(--radius-sm)',
                            display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                          }}>
                          <span>{v.id}</span>
                          <span style={{ fontSize: 10, opacity: 0.6 }}>{v.type}</span>
                        </div>
                      ))
                    )}
                  </div>
                </div>

                <div>
                  <label className="input-group__label" style={{ display: 'block', marginBottom: 'var(--space-sm)', fontWeight: 600, fontSize: 'var(--font-size-sm)' }}>
                    {t('create_instance.loader_label')}
                  </label>
                  <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 6, marginBottom: 'var(--space-sm)' }}>
                    {(['Vanilla', 'Fabric', 'Quilt', 'Forge', 'NeoForge'] as const).map((l) => (
                      <Button key={l} size="sm" variant={loaderType === l ? 'primary' : 'ghost'}
                        onClick={() => handleLoaderChange(l)} style={{ fontSize: '11px' }}>
                        {l === 'Vanilla' ? t('create_instance.loader_vanilla') : l === 'Fabric' ? t('create_instance.loader_fabric') : l === 'Quilt' ? t('create_instance.loader_quilt') : l === 'Forge' ? t('create_instance.loader_forge') : t('create_instance.loader_neoforge')}
                      </Button>
                    ))}
                  </div>
                  {loaderType !== 'Vanilla' && (
                    <div ref={loaderListRef} onScroll={handleLoaderScroll}
                      style={{ maxHeight: 168, overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
                      {loaderVersionsError ? (
                        <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                          {t('create_instance.loader_versions_failed', { loader: loaderType })}
                        </div>
                      ) : currentLoaderVersions.length === 0 && !loaderVersionsLoading ? (
                        <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                          {t('create_instance.loader_no_versions')}
                        </div>
                      ) : (
                        <>
                          {currentLoaderVersions.map((lv) => (
                            <div key={lv.version} onClick={() => setSelectedLoaderVersion(lv.version)}
                              style={{
                                padding: '6px 10px', cursor: 'pointer', fontSize: 'var(--font-size-sm)',
                                background: selectedLoaderVersion === lv.version ? 'var(--primary)' : 'transparent',
                                color: selectedLoaderVersion === lv.version ? 'white' : 'var(--text-primary)',
                                borderRadius: 'var(--radius-sm)',
                                display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                              }}>
                              <span>{lv.version}</span>
                              {lv.stable && <span style={{ fontSize: 10, opacity: 0.6 }}>{t('create_instance.stable_badge')}</span>}
                            </div>
                          ))}
                          {loaderVersionsLoading && <Skeleton height={32} style={{ margin: '4px 8px' }} />}
                          {!loaderVersionsLoading && !hasMoreLoaderVersions && currentLoaderTotal !== null && currentLoaderTotal > 0 && (
                            <div style={{ padding: '8px', textAlign: 'center', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                              {t('create_instance.loader_end', { loaded: currentLoaderVersions.length, total: currentLoaderTotal })}
                            </div>
                          )}
                        </>
                      )}
                      {currentLoaderVersions.length === 0 && loaderVersionsLoading && <Skeleton height={32} style={{ margin: '4px 8px' }} />}
                    </div>
                  )}
                  {loaderType === 'Vanilla' && (
                    <p style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)', marginTop: 'var(--space-sm)' }}>
                      {t('create_instance.vanilla_hint')}
                    </p>
                  )}
                </div>
              </div>
            </>
          ) : (
            /* Import mode */
            <div>
              <p style={{
                color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)',
                marginBottom: 'var(--space-lg)',
              }}>
                {t('create_instance.import_desc')}
              </p>

              <Button onClick={handlePickFile} variant="secondary" style={{ width: '100%', justifyContent: 'center', padding: 'var(--space-xl)' }}>
                <FileArchive size={20} />
                {importPath ? t('create_instance.import_change_file') : t('create_instance.import_select_file')}
              </Button>

              {importPath && (
                <div style={{
                  marginTop: 'var(--space-md)', padding: 'var(--space-sm) var(--space-md)',
                  background: 'var(--surface-glass)', borderRadius: 'var(--radius-md)',
                  fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)',
                  wordBreak: 'break-all',
                }}>
                  {importPath}
                </div>
              )}

              {importLoading && (
                <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', marginTop: 'var(--space-lg)' }}>
                  <LoadingSpinner />
                  <span style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>{t('create_instance.import_analyzing')}</span>
                </div>
              )}

              {importError && (
                <div style={{
                  marginTop: 'var(--space-lg)', padding: 'var(--space-md)',
                  background: 'var(--banner-error-bg)', border: '1px solid var(--banner-error-border)',
                  borderRadius: 'var(--radius-md)', fontSize: 'var(--font-size-sm)',
                  color: 'var(--error)',
                }}>
                  {importError}
                </div>
              )}

              {importProgress && (
                <div style={{
                  marginTop: 'var(--space-lg)',
                  background: 'var(--surface-glass)', border: '1px solid var(--surface-border)',
                  borderRadius: 'var(--radius-lg)', padding: 'var(--space-lg)',
                }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', marginBottom: 'var(--space-sm)' }}>
                    <LoadingSpinner />
                    <span style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, color: 'var(--text-primary)' }}>
                      {importProgress.message}
                    </span>
                  </div>
                  {importProgress.total > 1 && (
                    <div style={{
                      height: 6, borderRadius: 3, background: 'var(--surface-border)',
                      overflow: 'hidden', marginTop: 'var(--space-sm)',
                    }}>
                      <div style={{
                        height: '100%', borderRadius: 3,
                        background: 'var(--primary)',
                        width: `${Math.round((importProgress.current / importProgress.total) * 100)}%`,
                        transition: 'width 0.3s ease',
                      }} />
                    </div>
                  )}
                  <div style={{
                    fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)',
                    marginTop: 'var(--space-xs)', textAlign: 'right',
                  }}>
                    {importProgress.current} / {importProgress.total}
                  </div>
                </div>
              )}

              {importMeta && (
                <div style={{
                  marginTop: 'var(--space-lg)',
                  background: 'var(--surface-glass)', border: '1px solid var(--surface-border)',
                  borderRadius: 'var(--radius-lg)', padding: 'var(--space-lg)',
                }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', marginBottom: 'var(--space-md)' }}>
                    <Package size={20} style={{ color: 'var(--primary)' }} />
                    <div>
                      <div style={{ fontWeight: 600, fontSize: 'var(--font-size-md)' }}>{importMeta.name}</div>
                      <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)' }}>
                        {formatLabel(importMeta.format)}
                      </div>
                    </div>
                  </div>

                  <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)' }}>
                    {importMeta.mc_version && (
                      <>
                        <span style={{ color: 'var(--text-tertiary)' }}>{t('create_instance.meta_mc')}</span>
                        <span style={{ color: 'var(--text-primary)', fontWeight: 500 }}>{importMeta.mc_version}</span>
                      </>
                    )}
                    {importMeta.loader && (
                      <>
                        <span style={{ color: 'var(--text-tertiary)' }}>{t('create_instance.meta_loader')}</span>
                        <span style={{ color: 'var(--text-primary)', fontWeight: 500 }}>
                          {importMeta.loader}{importMeta.loader_version ? ` ${importMeta.loader_version}` : ''}
                        </span>
                      </>
                    )}
                    <span style={{ color: 'var(--text-tertiary)' }}>{t('create_instance.meta_format')}</span>
                    <span style={{ color: 'var(--text-primary)', fontWeight: 500 }}>{formatLabel(importMeta.format)}</span>
                  </div>

                  {importMeta.summary && (
                    <p style={{
                      marginTop: 'var(--space-md)', fontSize: 'var(--font-size-xs)',
                      color: 'var(--text-secondary)', lineHeight: 1.5,
                    }}>
                      {importMeta.summary}
                    </p>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </Modal>
  );
}
