import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { t, getLanguage } from '../lib/i18n';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { Search, FileArchive, Package, Plus, Upload, Compass } from 'lucide-react';
import { Modal } from '../components/ui/Modal';
import { Button } from '../components/ui/Button';
import { Input } from '../components/ui/Input';
import { Skeleton } from '../components/ui/Skeleton';
import { SnakeSpinner } from '../components/ui/SnakeSpinner';
import { useInstanceStore } from '../stores/instanceStore';
import { addToast } from '../components/ui/Toast';
import { useLogPlaque } from '../lib/uiLog';
import { SlidingTabs, type SlidingTabDef } from '../components/ui/SlidingTabs';
import { ImportReportPanel } from '../components/import/ImportReportPanel';
import { reportHasGaps, type ImportOutcome, type ImportReport } from '../lib/importReport';
import { needsLoaderVersionFetch } from '../lib/loaderVersions';
import { planCreateFailure, type CreateInstanceStage } from '../lib/createInstanceFlow';

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
  onOpenModpacks?: () => void;
  /** True after the modpack catalog was opened from this menu (highlight it
      as the current/active destination when the menu is shown again). */
  viaModpacks?: boolean;
  /** Clear the modpacks highlight when the user picks another menu item. */
  onClearViaModpacks?: () => void;
}

type LoaderType = 'Vanilla' | 'Fabric' | 'Forge' | 'NeoForge';
type VersionFilter = 'release' | 'snapshot' | 'all';

const LOADER_PAGE_SIZE = 20;
const SCROLL_LOAD_THRESHOLD_PX = 50;

/** A catalog request must never spin forever. The backend has its own
 *  retry/timeout budget, but a hung socket can outlive it; after this
 *  budget the well shows an explicit error with a retry button instead of
 *  an endless skeleton. */
const CATALOG_REQUEST_TIMEOUT_MS = 20_000;

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`Request timed out after ${Math.round(ms / 1000)}s`)),
      ms,
    );
    promise.then(
      (value) => { clearTimeout(timer); resolve(value); },
      (error) => { clearTimeout(timer); reject(error); },
    );
  });
}

type LoaderListState = {
  loading: boolean;
  error: string | null;
  total: number | null;
};

const emptyLoaderStates = (): Record<LoaderType, LoaderListState> => ({
  Vanilla: { loading: false, error: null, total: null },
  Fabric: { loading: false, error: null, total: null },
  Forge: { loading: false, error: null, total: null },
  NeoForge: { loading: false, error: null, total: null },
});

type WizardMode = 'new' | 'import';
type ModeTab = WizardMode | 'modpacks';

export function CreateInstanceWizard({ open, onClose, onOpenModpacks, viaModpacks = false, onClearViaModpacks }: CreateWizardProps) {
  const { createInstance, installVersion, loadInstances } = useInstanceStore();

  const [mode, setMode] = useState<WizardMode>('new');

  // New instance state
  const [versions, setVersions] = useState<VersionEntry[]>([]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState<string | null>(null);
  // Distinguishes "manifest has not arrived yet" from "manifest arrived but
  // carried no versions": both leave `selectedVersion` empty, but only the
  // first one may show a loading skeleton indefinitely.
  const [versionsLoaded, setVersionsLoaded] = useState(false);
  const [selectedVersion, setSelectedVersion] = useState('');
  const [versionFilter, setVersionFilter] = useState<VersionFilter>('release');
  const [searchQuery, setSearchQuery] = useState('');

  const [loaderType, setLoaderType] = useState<LoaderType>('Vanilla');
  const [loaderVersions, setLoaderVersions] = useState<Record<LoaderType, LoaderVersion[]>>({
    Vanilla: [], Fabric: [], Forge: [], NeoForge: [],
  });
  // Per-loader state: one loader failing (or hanging) must not mark the
  // others as failed, and switching loaders must never inherit a stale
  // "loading" flag.
  const [loaderListState, setLoaderListState] = useState<Record<LoaderType, LoaderListState>>(emptyLoaderStates);
  const [selectedLoaderVersion, setSelectedLoaderVersion] = useState('');

  const [instanceName, setInstanceName] = useState('');
  const [isCreating, setIsCreating] = useState(false);

  // Import state
  const [importPath, setImportPath] = useState<string | null>(null);
  const [importMeta, setImportMeta] = useState<ModpackMetadata | null>(null);
  const [importLoading, setImportLoading] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);
  const [importProgress, setImportProgress] = useState<{ current: number; total: number; stage: string; message: string } | null>(null);
  const [importReport, setImportReport] = useState<ImportReport | null>(null);
  const [importReportName, setImportReportName] = useState('');
  const [retryingImport, setRetryingImport] = useState(false);

  useLogPlaque(importError, 'error', 'import');
  useLogPlaque(importProgress?.message ?? null, 'info', 'import');

  const loaderListRef = useRef<HTMLDivElement>(null);
  // Each loader-list request gets a sequence number per loader. Changing the
  // Minecraft version (or loader) while a page is still downloading fires a
  // new request; without this guard the older answer landed afterwards and
  // mixed versions from the previous game version into the list.
  const loaderRequestIds = useRef<Record<LoaderType, number>>({
    Vanilla: 0, Fabric: 0, Forge: 0, NeoForge: 0,
  });
  // In-flight flags are per loader too: a slow Forge request must not block
  // Fabric/NeoForge from loading, and a reset request must always be allowed
  // to supersede a hung one.
  const loaderInFlight = useRef<Record<LoaderType, boolean>>({
    Vanilla: false, Fabric: false, Forge: false, NeoForge: false,
  });
  const versionsRequestRef = useRef(0);

  // The dialog must occupy exactly the same frame in both modes (owner: «размер
  // окна … должен быть одинаковый» — a jumping dialog reads as broken). While
  // «Новая» is on screen this pin tracks the row's laid-out box (the versions
  // list fills in asynchronously, so it is frozen on every layout change);
  // «Импорт» then reuses the last frozen box as its minimum, which keeps the
  // dialog from resizing when the mode switches.
  const rowRef = useRef<HTMLDivElement>(null);
  const pinnedSize = useRef<{ w: number; h: number } | null>(null);

  useLayoutEffect(() => {
    if (!open || mode !== 'new') return;
    const el = rowRef.current;
    if (!el) return;
    const freeze = () => { pinnedSize.current = { w: el.offsetWidth, h: el.offsetHeight }; };
    freeze();
    if (typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(freeze);
    ro.observe(el);
    return () => ro.disconnect();
  }, [open, mode]);

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
    setVersionsError(null);
    setVersionsLoaded(false);
    setLoaderListState(emptyLoaderStates());
    setImportPath(null);
    setImportMeta(null);
    setImportLoading(false);
    setImportError(null);
    setImportProgress(null);
    setImportReport(null);
    setImportReportName('');
    setRetryingImport(false);
  };

  const fetchVersions = async () => {
    const reqId = ++versionsRequestRef.current;
    setVersionsLoading(true);
    setVersionsError(null);
    setVersionsLoaded(false);
    try {
      const manifest = await withTimeout(
        invoke<{ versions: VersionEntry[] }>('cmd_get_versions'),
        CATALOG_REQUEST_TIMEOUT_MS,
      );
      if (reqId !== versionsRequestRef.current) return;
      setVersions(manifest.versions);
      setVersionsLoaded(true);
      if (manifest.versions.length > 0) {
        const fallback = manifest.versions.find((v) => v.type === 'release')?.id || manifest.versions[0].id;
        setSelectedVersion((prev) =>
          prev && manifest.versions.some((v) => v.id === prev) ? prev : fallback,
        );
      }
    } catch (e) {
      if (reqId !== versionsRequestRef.current) return;
      console.error('Failed to fetch versions:', e);
      setVersionsError(String(e));
    } finally {
      if (reqId === versionsRequestRef.current) setVersionsLoading(false);
    }
  };

  const fetchLoaderPage = useCallback(async (loader?: LoaderType, resetOffset = false, mcVersionOverride?: string) => {
    const target: LoaderType = loader ?? loaderType;
    if (target === 'Vanilla') return;
    const mcVersion = mcVersionOverride ?? selectedVersion;
    if (mcVersion === '') {
      // The Minecraft version list is still loading. Leave the total unset so
      // the retry effect below requests the loader page once it arrives.
      return;
    }
    if (!resetOffset && loaderInFlight.current[target]) return;
    const offset = resetOffset ? 0 : (loaderVersions[target]?.length ?? 0);
    const totalKnown = loaderListState[target].total;
    if (!resetOffset && totalKnown !== null && offset >= totalKnown) return;
    // A reset supersedes any request still running for this loader.
    if (resetOffset) loaderRequestIds.current[target] += 1;
    const reqId = loaderRequestIds.current[target];
    loaderInFlight.current[target] = true;
    setLoaderListState((prev) => ({
      ...prev,
      [target]: { ...prev[target], loading: true, error: null },
    }));
    try {
      let page: LoaderVersionPage;
      const args = { mcVersion, offset, limit: LOADER_PAGE_SIZE };
      if (target === 'Fabric') {
        page = await withTimeout(
          invoke<LoaderVersionPage>('cmd_get_fabric_versions', args),
          CATALOG_REQUEST_TIMEOUT_MS,
        );
      } else if (target === 'Forge') {
        page = await withTimeout(
          invoke<LoaderVersionPage>('cmd_get_forge_versions', args),
          CATALOG_REQUEST_TIMEOUT_MS,
        );
      } else if (target === 'NeoForge') {
        page = await withTimeout(
          invoke<LoaderVersionPage>('cmd_get_neoforge_versions', args),
          CATALOG_REQUEST_TIMEOUT_MS,
        );
      } else {
        page = { versions: [], total: 0 };
      }
      if (reqId !== loaderRequestIds.current[target]) return;
      setLoaderVersions(prev => ({
        ...prev,
        [target]: offset === 0 ? page.versions : [...(prev[target] ?? []), ...page.versions],
      }));
      setLoaderListState(prev => ({
        ...prev,
        [target]: { loading: false, error: null, total: page.total },
      }));
      if (offset === 0 && page.versions.length > 0) {
        setSelectedLoaderVersion(page.versions[0].version);
      }
    } catch (e) {
      if (reqId !== loaderRequestIds.current[target]) return;
      console.error('Failed to fetch loader versions:', e);
      setLoaderListState(prev => ({
        ...prev,
        [target]: { ...prev[target], loading: false, error: String(e) },
      }));
    } finally {
      if (reqId === loaderRequestIds.current[target]) loaderInFlight.current[target] = false;
    }
  }, [loaderType, selectedVersion, loaderVersions, loaderListState]);

  // If the user picks a loader before the Minecraft version list finishes
  // loading, the initial request above is skipped. Retry once the version is
  // known; a completed page (even an empty one) or a failure stops the retry.
  useEffect(() => {
    if (!open || mode !== 'new') return;
    const state = loaderListState[loaderType];
    if (!needsLoaderVersionFetch({
      loader: loaderType,
      mcVersion: selectedVersion,
      total: state.total,
      loading: state.loading,
      failed: state.error !== null,
    })) return;
    fetchLoaderPage(loaderType, true);
  }, [
    open,
    mode,
    loaderType,
    selectedVersion,
    loaderListState,
    fetchLoaderPage,
  ]);

  /** Abandon every in-flight loader request (they belong to the old
   *  Minecraft version / loader). Their late answers are dropped by the
   *  per-loader request id check. */
  const invalidateLoaderRequests = () => {
    loaderRequestIds.current.Fabric += 1;
    loaderRequestIds.current.Forge += 1;
    loaderRequestIds.current.NeoForge += 1;
    loaderInFlight.current = { Vanilla: false, Fabric: false, Forge: false, NeoForge: false };
  };

  const handleVersionSelect = (id: string) => {
    setSelectedVersion(id);
    setSelectedLoaderVersion('');
    invalidateLoaderRequests();
    // Loader lists are version-specific — reset them all
    setLoaderVersions({ Vanilla: [], Fabric: [], Forge: [], NeoForge: [] });
    setLoaderListState(emptyLoaderStates());
    if (loaderType !== 'Vanilla') fetchLoaderPage(loaderType, true, id);
  };

  const handleLoaderChange = (loader: LoaderType) => {
    setLoaderType(loader);
    setSelectedLoaderVersion('');
    invalidateLoaderRequests();
    if (loader === 'Vanilla') return;
    // Reuse a page already fetched for this Minecraft version: switching
    // Forge → Fabric → Forge must be instant, not a new network round-trip.
    const cached = loaderVersions[loader] ?? [];
    const known = loaderListState[loader].total;
    if (known !== null) {
      setSelectedLoaderVersion(cached[0]?.version ?? '');
      return;
    }
    setLoaderVersions(prev => ({ ...prev, [loader]: [] }));
    setLoaderListState(prev => ({
      ...prev,
      [loader]: { loading: false, error: null, total: null },
    }));
    fetchLoaderPage(loader, true);
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

  // Rebuilt per render so the labels always follow the active language.
  const versionFilterOptions: SlidingTabDef<VersionFilter>[] = [
    { id: 'all', label: t('create_instance.filter_all') },
    { id: 'release', label: t('create_instance.filter_release') },
    { id: 'snapshot', label: t('create_instance.filter_snapshot') },
  ];
  const loaderOptions: SlidingTabDef<LoaderType>[] = [
    { id: 'Vanilla', label: t('create_instance.loader_vanilla') },
    { id: 'Fabric', label: t('create_instance.loader_fabric') },
    { id: 'Forge', label: t('create_instance.loader_forge') },
    { id: 'NeoForge', label: t('create_instance.loader_neoforge') },
  ];

  const currentLoaderState = loaderListState[loaderType];
  const currentLoaderVersions = loaderVersions[loaderType] ?? [];
  const currentLoaderTotal = currentLoaderState.total;
  const hasMoreLoaderVersions = currentLoaderTotal === null || currentLoaderVersions.length < currentLoaderTotal;
  // The loader list cannot start until the Minecraft manifest provides a
  // version. Three states must not be conflated:
  //  * manifest still loading             → skeleton;
  //  * manifest failed or came back empty → explanation + retry;
  //  * manifest ready                     → skeleton only while this loader's
  //                                         own request is running.
  const mcVersionsUnavailable =
    versionsError !== null || (versionsLoaded && versions.length === 0);
  const awaitingMcVersions = versionsLoading || (!mcVersionsUnavailable && !versionsLoaded);
  const loaderSkeleton = !currentLoaderState.error
    && (awaitingMcVersions || currentLoaderState.loading || currentLoaderState.total === null);

  // New instance validation
  const canCreate = mode === 'new' && instanceName.trim() !== '' && selectedVersion !== '' && !isCreating
    && (loaderType === 'Vanilla' || selectedLoaderVersion !== '');

  // Import validation
  const canImport = mode === 'import' && instanceName.trim() !== '' && importMeta !== null && !isCreating;

  /**
   * Report a create/install failure without inviting a duplicate creation.
   * Once the instance directory exists, retrying "Create" can only report
   * "already exists", so the wizard closes and the toast points at Verify &
   * Repair instead.
   */
  const reportCreateFailure = async (
    stage: CreateInstanceStage,
    error: unknown,
    existedBefore: boolean,
  ) => {
    const name = instanceName.trim();
    // Refresh first: a create error may still have written the directory.
    try {
      await loadInstances();
    } catch {
      // The store already reports loading errors; never block the toast.
    }
    const existsNow = useInstanceStore.getState().instances.some(
      (instance) => instance.name === name,
    );
    // Only a directory that appeared during this attempt is a partial
    // success. A name that was already taken is a real error and the user
    // must choose another name, so the wizard stays open.
    const createdByThisAttempt = stage !== 'create' || (!existedBefore && existsNow);
    const plan = planCreateFailure(stage, createdByThisAttempt);

    setIsCreating(false);
    addToast(t(plan.toastKey, { name, error: String(error) }), 'error');
    if (plan.closeWizard) onClose();
  };

  const handleCreate = async () => {
    if (!canCreate) return;
    const name = instanceName.trim();
    // Baseline for the partial-success decision: an instance with this name
    // that exists BEFORE the attempt was not created by it.
    const existedBefore = useInstanceStore
      .getState()
      .instances.some((instance) => instance.name === name);
    setIsCreating(true);
    try {
      const versionEntry = versions.find((v) => v.id === selectedVersion);
      if (!versionEntry) throw new Error('Version not found');
      addToast(t('create_instance.downloading'), 'info');
      const ldr = loaderType === 'Vanilla' ? undefined : loaderType;
      const ldrVer = loaderType === 'Vanilla' ? undefined : selectedLoaderVersion;

      try {
        await createInstance(name, selectedVersion, ldr, ldrVer);
      } catch (e: any) {
        await reportCreateFailure('create', e, existedBefore);
        return;
      }

      try {
        await installVersion(versionEntry.url, name);
      } catch (e: any) {
        // The instance exists and is safe; only its game files are missing.
        // Never delete it here — close the wizard and let the user finish
        // from the instance page (Verify & Repair).
        await reportCreateFailure('game-files', e, existedBefore);
        return;
      }

      if (loaderType !== 'Vanilla' && selectedLoaderVersion) {
        const cmd = loaderType === 'Fabric' ? 'cmd_install_fabric'
          : loaderType === 'Forge' ? 'cmd_install_forge'
          : 'cmd_install_neoforge';
        addToast(t('create_instance.loader_installing', { loader: loaderType }), 'info');
        try {
          await invoke(cmd, { mcVersion: selectedVersion, loaderVersion: selectedLoaderVersion, instanceName: name, lang: getLanguage() });
        } catch (e: any) {
          await reportCreateFailure('loader', e, existedBefore);
          return;
        }
      }

      addToast(t('create_instance.created', { name }), 'success');
      await loadInstances();
      onClose();
    } catch (e: any) {
      // Failure before/around instance creation (e.g. version lookup).
      await reportCreateFailure('create', e, existedBefore);
    } finally {
      setIsCreating(false);
    }
  };

  const handlePickFile = async () => {
    try {
      const selected = await openFileDialog({
        multiple: false,
        filters: [
          { name: t('create_instance.filter_modpack_archives'), extensions: ['zip', 'mrpack'] },
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
    setImportReport(null);
    setImportProgress({ current: 0, total: 1, stage: 'starting', message: t('create_instance.import_starting') });
    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await listen<{ current: number; total: number; stage: string; message: string }>('import-progress', (event) => {
        setImportProgress(event.payload);
      });
      const outcome = await invoke<ImportOutcome>('cmd_import_modpack', {
        path: importPath,
        instanceName: instanceName.trim(),
      });
      await loadInstances();
      if (reportHasGaps(outcome.report)) {
        // Keep the dialog open: the user must see which files are missing
        // and can retry them right here.
        setImportReport(outcome.report);
        setImportReportName(outcome.instance.name || instanceName.trim());
        addToast(t('create_instance.imported_partial', {
          installed: String(outcome.report.installed),
          total: String(outcome.report.total_files),
        }), 'warning');
      } else if (outcome.report.warnings.length > 0) {
        addToast(t('create_instance.imported_incomplete', { name: instanceName }), 'warning');
        onClose();
      } else {
        addToast(t('create_instance.imported', { name: instanceName }), 'success');
        onClose();
      }
    } catch (e: any) {
      addToast(t('create_instance.failed', { error: e.toString() }), 'error');
    }
    if (unlisten) unlisten();
    setIsCreating(false);
    setImportProgress(null);
  };

  const handleRetryImport = async () => {
    if (!importReport || retryingImport) return;
    const retryable = importReport.failed.filter(
      (f) => f.retryable && f.project_id != null && f.file_id != null,
    );
    if (retryable.length === 0) return;
    setRetryingImport(true);
    try {
      const result = await invoke<ImportReport>('cmd_retry_curseforge_import_files', {
        instanceName: importReportName,
        files: retryable.map((f) => ({ projectId: f.project_id, fileId: f.file_id })),
      });
      const stillFailed = [
        ...importReport.failed.filter((f) => !retryable.includes(f)),
        ...result.failed,
      ];
      const installed = importReport.installed + result.installed;
      const updated = { ...importReport, installed, failed: stillFailed };
      if (!reportHasGaps(updated)) {
        setImportReport(null);
        addToast(t('create_instance.imported_retry_done'), 'success');
        await loadInstances();
        onClose();
      } else {
        setImportReport(updated);
        if (result.failed.length > 0) {
          addToast(t('modpacks.retry_partial', {
            installed: String(result.installed),
            failed: String(result.failed.length),
          }), 'warning');
        } else if (importReport.skipped > 0) {
          addToast(t('modpacks.retry_done_skipped', { n: String(importReport.skipped) }), 'warning');
        }
        await loadInstances();
      }
    } catch (e: any) {
      addToast(t('modpacks.retry_error', { error: e.toString() }), 'error');
    } finally {
      setRetryingImport(false);
    }
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
      growWithContent
      dense
      footer={
        <div style={{ display: 'flex', justifyContent: 'space-between', width: '100%' }}>
          <Button variant="ghost" onClick={onClose}>{t('common.cancel')}</Button>
          {mode === 'new' ? (
            <Button onClick={handleCreate} loading={isCreating} disabled={!canCreate}>
              <Plus size={14} />
              {t('create_instance.title')}
            </Button>
          ) : (
            <Button onClick={handleImport} loading={isCreating} disabled={!canImport || !!importReport}>
              <Upload size={14} />
              {t('create_instance.import_action')}
            </Button>
          )}
        </div>
      }
    >
      {/* Mode selector sidebar + content */}
      <div ref={rowRef} style={{
        display: 'flex', gap: 'var(--space-xl)', minHeight: 360,
        ...(mode === 'import' && pinnedSize.current
          ? { minWidth: pinnedSize.current.w, minHeight: pinnedSize.current.h }
          : null),
      }}>
        {/* Left sidebar — mode select: the same tab language as the terminal
            and the modpack catalog (transparent labels, one frame that
            physically slides down the column). */}
        <div className="create-wizard__mode" style={{
          width: 140, flexShrink: 0,
          borderRight: '1px solid var(--surface-border)', paddingRight: 'var(--space-lg)',
        }}>
          <SlidingTabs<ModeTab>
            orientation="vertical"
            tabs={[
              { id: 'new', label: <><Plus size={16} /> {t('create_instance.mode_new')}</> },
              { id: 'modpacks', label: <><Compass size={16} /> {t('modpacks.title')}</> },
              { id: 'import', label: <><Upload size={16} /> {t('create_instance.mode_import')}</> },
            ]}
            active={viaModpacks ? 'modpacks' : mode}
            onChange={(id) => {
              if (id === 'modpacks') onOpenModpacks?.();
              else { setMode(id); onClearViaModpacks?.(); }
            }}
            ariaLabel={t('create_instance.title')}
            style={{ gap: 'var(--space-sm)', marginBottom: 0 }}
          />
        </div>

        {/* Right content area */}
        <div style={{ flex: 1, minWidth: 0 }}>
          {/* Instance Name */}
          <div style={{ marginBottom: 'var(--space-lg)' }}>
            <Input
              label={t('create_instance.name_label')}
              id="wizard-name"
              name="wizard-instance-name-new"
              autoComplete="off"
              type="text"
              value={instanceName}
              onChange={(e) => setInstanceName(e.target.value)}
              autoFocus={mode === 'new'}
            />
          </div>

          {mode === 'new' ? (
            <>
              {/* MC Version + Loader in two columns */}
              {/* minmax(0, 1fr) for the version column (floors below content
                  min-width) and `auto` for the loader column: the switcher row
                  keeps its natural width whatever the font metrics, and the
                  version column absorbs the slack — a wide 4-loader row can no
                  longer push "NeoForge" past the dialog edge. */}
              <div style={{ display: 'grid', gridTemplateColumns: 'minmax(0, 1fr) auto', gap: 'var(--space-xl)' }}>
                <div>
                  <label className="input-group__label" style={{ display: 'block', marginBottom: 'var(--space-sm)', fontWeight: 600, fontSize: 'var(--font-size-sm)' }}>
                    {t('create_instance.version_label')}
                  </label>
                  <div style={{ position: 'relative', marginBottom: 'var(--space-sm)' }}>
                    <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--text-tertiary)', zIndex: 1, pointerEvents: 'none' }} />
                    <input className="input" type="text"
                      value={searchQuery} onChange={(e) => setSearchQuery(e.target.value)}
                      placeholder={t('create_instance.version_search_placeholder')}
                      aria-label={t('create_instance.version_search_placeholder')}
                      style={{ paddingLeft: 32, fontSize: 'var(--font-size-sm)' }} />
                  </div>
                  <SlidingTabs<VersionFilter>
                    tabs={versionFilterOptions}
                    active={versionFilter}
                    onChange={setVersionFilter}
                    ariaLabel={t('create_instance.version_label')}
                    className="wizard-switch"
                    style={{ marginBottom: 'var(--space-sm)', borderBottom: 'none' }}
                  />
                  {/* Re-keyed on the filter so switching release/snapshot/all
                      fades the new rows in instead of swapping them silently.
                      The well has a fixed height (not maxHeight): the frame
                      must not resize when the filter changes, the loader
                      changes or the search narrows the list (owner:
                      «неприятно видеть как постоянно меняется размер окна»).
                      The height still tracks the viewport so short windows
                      fit without the dialog itself needing to scroll. */}
                  <div key={versionFilter} className="animate-content-reveal version-list" style={{ height: 'min(200px, max(64px, calc(85vh - 447px)))', overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
                    {versionsLoading ? (
                      Array.from({ length: 6 }).map((_, i) => <Skeleton key={i} height={32} style={{ marginBottom: 2 }} />)
                    ) : versionsError && versions.length === 0 ? (
                      <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                        {t('create_instance.versions_failed')}
                        <div style={{ marginTop: 4 }}>{t('create_instance.versions_failed_hint')}</div>
                        <Button variant="secondary" size="sm" onClick={fetchVersions} style={{ marginTop: 8 }}>
                          {t('create_instance.versions_retry')}
                        </Button>
                      </div>
                    ) : filteredVersions.length === 0 ? (
                      <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                        {t('create_instance.versions_empty')}
                      </div>
                    ) : (
                      filteredVersions.map((v) => (
                        <button
                          key={v.id}
                          type="button"
                          className={`version-item${selectedVersion === v.id ? ' version-item--selected' : ''}`}
                          aria-pressed={selectedVersion === v.id}
                          title={v.id}
                          onClick={() => handleVersionSelect(v.id)}
                        >
                          <span className="version-item__name">{v.id}</span>
                          <span className="version-item__type">{v.type}</span>
                        </button>
                      ))
                    )}
                  </div>
                </div>

                <div>
                  <label className="input-group__label" style={{ display: 'block', marginBottom: 'var(--space-sm)', fontWeight: 600, fontSize: 'var(--font-size-sm)' }}>
                    {t('create_instance.loader_label')}
                  </label>
                  <SlidingTabs<LoaderType>
                    tabs={loaderOptions}
                    active={loaderType}
                    onChange={handleLoaderChange}
                    ariaLabel={t('create_instance.loader_label')}
                    className="wizard-switch"
                    style={{ marginBottom: 'var(--space-sm)', borderBottom: 'none' }}
                  />
                  {loaderType !== 'Vanilla' && (
                    /* Fixed-height well like the version list. `width: 0;
                       min-width: 100%` keeps the well from contributing its
                       intrinsic width to the `auto` grid column: the column
                       stays sized by the switcher row, so long loader ids
                       never widen it or squeeze the version column. */
                    <div key={`${loaderType}-${selectedVersion}`} ref={loaderListRef} onScroll={handleLoaderScroll}
                      className="animate-content-reveal version-list"
                      style={{ height: 'min(168px, max(56px, calc(85vh - 447px)))', width: 0, minWidth: '100%', overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
                      {versionsError && selectedVersion === '' ? (
                        <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                          {t('create_instance.loader_blocked_by_versions', { loader: loaderType })}
                          <div style={{ marginTop: 4 }}>{t('create_instance.loader_blocked_hint')}</div>
                          <Button variant="secondary" size="sm" onClick={fetchVersions} style={{ marginTop: 8 }}>
                            {t('create_instance.versions_retry')}
                          </Button>
                        </div>
                      ) : versionsLoaded && versions.length === 0 ? (
                        <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                          {t('create_instance.versions_empty')}
                          <div style={{ marginTop: 4 }}>{t('create_instance.loader_blocked_hint')}</div>
                          <Button variant="secondary" size="sm" onClick={fetchVersions} style={{ marginTop: 8 }}>
                            {t('create_instance.versions_retry')}
                          </Button>
                        </div>
                      ) : currentLoaderState.error ? (
                        <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                          {t('create_instance.loader_versions_failed', { loader: loaderType })}
                          <div style={{ marginTop: 4, wordBreak: 'break-word' }}>{currentLoaderState.error}</div>
                          <Button
                            variant="secondary"
                            size="sm"
                            onClick={() => fetchLoaderPage(loaderType, true)}
                            style={{ marginTop: 8 }}
                          >
                            {t('create_instance.versions_retry')}
                          </Button>
                        </div>
                      ) : loaderSkeleton ? (
                        Array.from({ length: 3 }).map((_, i) => (
                          <Skeleton key={i} height={32} style={{ margin: '2px 8px' }} />
                        ))
                      ) : currentLoaderVersions.length === 0 ? (
                        <div style={{ padding: '12px', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                          {t('create_instance.loader_no_versions', { loader: loaderType, version: selectedVersion })}
                          <div style={{ marginTop: 4 }}>{t('create_instance.loader_unsupported_hint')}</div>
                        </div>
                      ) : (
                        <>
                          {currentLoaderVersions.map((lv) => (
                            <button
                              key={lv.version}
                              type="button"
                              className={`version-item${selectedLoaderVersion === lv.version ? ' version-item--selected' : ''}`}
                              aria-pressed={selectedLoaderVersion === lv.version}
                              title={lv.version}
                              onClick={() => setSelectedLoaderVersion(lv.version)}
                            >
                              <span className="version-item__name">{lv.version}</span>
                              {lv.stable && <span className="version-item__type">{t('create_instance.stable_badge')}</span>}
                            </button>
                          ))}
                          {currentLoaderState.loading && <Skeleton height={32} style={{ margin: '4px 8px' }} />}
                          {!currentLoaderState.loading && !hasMoreLoaderVersions && currentLoaderTotal !== null && currentLoaderTotal > 0 && (
                            <div style={{ padding: '8px', textAlign: 'center', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                              {t('create_instance.loader_end', { loaded: currentLoaderVersions.length, total: currentLoaderTotal })}
                            </div>
                          )}
                        </>
                      )}
                    </div>
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
                  <SnakeSpinner size={18} />
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
                    <SnakeSpinner size={18} />
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

              {importReport && (
                <ImportReportPanel
                  report={importReport}
                  instanceName={importReportName}
                  retrying={retryingImport}
                  onRetry={handleRetryImport}
                />
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

                  <div style={{ display: 'grid', gridTemplateColumns: 'minmax(0, 1fr) minmax(0, 1fr)', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)' }}>
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
