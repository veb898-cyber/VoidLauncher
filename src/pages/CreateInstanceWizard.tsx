import { useCallback, useEffect, useRef, useState } from 'react';
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

/// Backend pagination shape — see `prism_meta::LoaderVersionPage`
/// in `modloaders/prism_meta.rs`. The wizard asks for one page at
/// a time and uses `total` to know when to stop requesting more.
interface LoaderVersionPage {
  versions: LoaderVersion[];
  total: number;
}

interface CreateWizardProps {
  open: boolean;
  onClose: () => void;
}

type LoaderType = 'Vanilla' | 'Fabric' | 'Quilt' | 'Forge' | 'NeoForge' | 'LiteLoader';

/// How many loader versions to ask for per page. Must match the
/// Rust-side `PAGE_SIZE` constant in `lib.rs` (the backend clamps
/// `limit = 0` to this value, so passing 0 here also works, but
/// passing the explicit value keeps the intent obvious).
const LOADER_PAGE_SIZE = 20;

/// Scroll distance from the bottom (in px) at which to trigger
/// loading the next page. 50px is roughly two list rows at the
/// current `font-size-sm` + `padding: 6px 10px` — close enough to
/// "the user reached the end" without firing during a small fling
/// that doesn't actually need a fetch.
const SCROLL_LOAD_THRESHOLD_PX = 50;

export function CreateInstanceWizard({ open, onClose }: CreateWizardProps) {
  const { createInstance, installVersion, saveInstance } = useInstanceStore();

  const [versions, setVersions] = useState<VersionEntry[]>([]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [selectedVersion, setSelectedVersion] = useState('');
  const [versionFilter, setVersionFilter] = useState<'release' | 'snapshot' | 'all'>('release');
  const [searchQuery, setSearchQuery] = useState('');

  const [loaderType, setLoaderType] = useState<LoaderType>('Vanilla');
  /// Per-loader version accumulators (the wizard's infinite-scroll
  /// state). Keyed by loader name so switching back to a loader the
  /// user has already paginated through doesn't reset their scroll
  /// position. `Vanilla` is never read (no versions) but we keep it
  /// in the type for the `Record<LoaderType, ...>` shape.
  const [loaderVersions, setLoaderVersions] = useState<Record<LoaderType, LoaderVersion[]>>({
    Vanilla: [], Fabric: [], Quilt: [], Forge: [], NeoForge: [], LiteLoader: [],
  });
  /// Per-loader `total` from the most recent successful page. The
  /// wizard stops requesting more pages when `accumulated.length
  /// >= total`. `null` means "haven't fetched yet for this loader".
  const [loaderVersionTotals, setLoaderVersionTotals] = useState<Record<LoaderType, number | null>>({
    Vanilla: null, Fabric: null, Quilt: null, Forge: null, NeoForge: null, LiteLoader: null,
  });
  const [loaderVersionsLoading, setLoaderVersionsLoading] = useState(false);
  const [loaderVersionsError, setLoaderVersionsError] = useState(false);
  const [selectedLoaderVersion, setSelectedLoaderVersion] = useState('');

  const [instanceName, setInstanceName] = useState('');
  const [isCreating, setIsCreating] = useState(false);

  /// Ref to the loader-version scrollable container. We read
  /// `scrollHeight` / `scrollTop` / `clientHeight` on scroll to
  /// decide whether to load the next page. Using a ref (rather than
  /// the React event's `currentTarget`) avoids stale-closure issues
  /// if the user re-opens the wizard with a different loader.
  const loaderListRef = useRef<HTMLDivElement>(null);

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
    setLoaderVersionsLoading(false);
    setLoaderVersionsError(false);
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

  /// Fetch the next page of loader versions for `loader` and append
  /// them to its accumulator. If `loader` is omitted, uses the
  /// currently active loader (used by the scroll handler; the
  /// button-click path passes the loader explicitly to avoid
  /// reading a stale `loaderType` between `setLoaderType` and the
  /// React re-render).
  const fetchLoaderPage = useCallback(async (loader?: LoaderType) => {
    const target: LoaderType = loader ?? loaderType;
    if (target === 'Vanilla') return;

    if (loaderVersionsLoading) return;

    /// Guard: if the MC version list hasn't loaded yet, `selectedVersion`
    /// is `''` and the Rust filter `requires.equals == ""` would
    /// return 0 matches for every loader that needs an MC filter
    /// (Forge / NeoForge / LiteLoader). Bail out with an empty
    /// accumulator instead of sending a bogus mcVersion.
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
        page = await invoke<LoaderVersionPage>('cmd_get_liteloader_versions', { mcVersion: selectedVersion, offset, limit: LOADER_PAGE_SIZE });
      }

      setLoaderVersions(prev => ({
        ...prev,
        [target]: [...(prev[target] ?? []), ...page.versions],
      }));
      setLoaderVersionTotals(prev => ({ ...prev, [target]: page.total }));

      /// If this was the first page (offset was 0) and at least
      /// one version came back, pre-select the newest one (which
      /// is `page.versions[0]` — see the `sort_by(b.version.cmp)`
      /// newest-first in prism_meta). This matches the pre-scroll
      /// behavior: clicking a loader button should leave the user
      /// on a sensible default.
      if (offset === 0 && page.versions.length > 0) {
        setSelectedLoaderVersion(page.versions[0].version);
      }
    } catch (e) {
      console.error('Failed to fetch loader versions:', e);
      setLoaderVersionsError(true);
    }
    setLoaderVersionsLoading(false);
  }, [loaderType, selectedVersion, loaderVersionsLoading, loaderVersions, loaderVersionTotals]);

  /// Called when the user clicks a loader button. Resets the
  /// accumulator for `loader` and starts the infinite scroll at
  /// page 0.
  const handleLoaderChange = (loader: LoaderType) => {
    setLoaderType(loader);
    setSelectedLoaderVersion('');
    setLoaderVersionsError(false);
    if (loader === 'Vanilla') {
      setLoaderVersionsLoading(false);
      return;
    }
    /// Reset the accumulator for the freshly-clicked loader so we
    /// start from page 0 (not from whatever the previous loader
    /// had accumulated).
    setLoaderVersions(prev => ({ ...prev, [loader]: [] }));
    setLoaderVersionTotals(prev => ({ ...prev, [loader]: null }));
    fetchLoaderPage(loader);
  };

  /// Scroll handler: when the user is within 50px of the bottom of
  /// the loader-version list, load the next page. No-op while a
  /// fetch is already in flight or when we've already loaded the
  /// full list.
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
  const hasMoreLoaderVersions = currentLoaderTotal === null
    || currentLoaderVersions.length < currentLoaderTotal;

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
          : loaderType === 'LiteLoader' ? 'cmd_install_liteloader'
          : 'cmd_install_neoforge';
        try {
          await invoke(cmd, { mcVersion: selectedVersion, loaderVersion: selectedLoaderVersion, instanceName: instanceName.trim() });
        } catch (e: any) {
          // LiteLoader's install is intentionally a hard error (the
          // upstream download URLs are dead) — surface the backend's
          // message verbatim rather than the generic "loader_failed".
          if (loaderType === 'LiteLoader') {
            addToast(t('create_instance.liteloader_unsupported', { error: String(e) }), 'warning');
          } else {
            addToast(t('create_instance.loader_failed', { loader: loaderType }), 'warning');
          }
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
        {/* Left: Version Selection — show ALL versions, no pagination.
            The container is `maxHeight: 200, overflowY: auto` so the
            user can scroll through the full list (no slice cap). */}
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
              filteredVersions.map((v) => (
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

        {/* Right: Loader Selection — infinite scroll, 20 items per
            page. The container is `maxHeight: 168, overflowY: auto`
            and we load the next page when the user is within
            50px of the bottom (see `handleLoaderScroll`). */}
        <div>
          <label className="input-group__label" style={{ display: 'block', marginBottom: 'var(--space-sm)', fontWeight: 600, fontSize: 'var(--font-size-sm)' }}>
            {t('create_instance.loader_label')}
          </label>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 6, marginBottom: 'var(--space-sm)' }}>
            {(['Vanilla', 'Fabric', 'Quilt', 'Forge', 'NeoForge', 'LiteLoader'] as const).map((l) => (
              <Button
                key={l}
                size="sm"
                variant={loaderType === l ? 'primary' : 'ghost'}
                onClick={() => handleLoaderChange(l)}
                style={{ fontSize: '11px' }}
              >
                {l === 'Vanilla' ? t('create_instance.loader_vanilla') : l === 'Fabric' ? t('create_instance.loader_fabric') : l === 'Quilt' ? t('create_instance.loader_quilt') : l === 'Forge' ? t('create_instance.loader_forge') : l === 'LiteLoader' ? t('create_instance.loader_liteloader') : t('create_instance.loader_neoforge')}
              </Button>
            ))}
          </div>

          {loaderType !== 'Vanilla' && (
            <div
              ref={loaderListRef}
              onScroll={handleLoaderScroll}
              style={{ maxHeight: 168, overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}
            >
              {loaderVersionsError ? (
                <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                  {t('create_instance.loader_versions_failed', { loader: loaderType })}
                </div>
              ) : currentLoaderVersions.length === 0 && !loaderVersionsLoading ? (
                <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                  {loaderType === 'LiteLoader'
                    ? t('create_instance.liteloader_empty')
                    : t('create_instance.loader_no_versions')}
                </div>
              ) : (
                <>
                  {currentLoaderVersions.map((lv) => (
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
                  ))}
                  {loaderVersionsLoading && (
                    <Skeleton height={32} style={{ margin: '4px 8px' }} />
                  )}
                  {!loaderVersionsLoading && !hasMoreLoaderVersions && currentLoaderTotal !== null && currentLoaderTotal > 0 && (
                    <div style={{ padding: '8px', textAlign: 'center', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                      {t('create_instance.loader_end', { loaded: currentLoaderVersions.length, total: currentLoaderTotal })}
                    </div>
                  )}
                </>
              )}
              {currentLoaderVersions.length === 0 && loaderVersionsLoading && (
                <Skeleton height={32} style={{ margin: '4px 8px' }} />
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
