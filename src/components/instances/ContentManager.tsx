import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { Package, FolderOpen, Download, Search, Check, Trash2, RefreshCw, ArrowRight, X, AlertTriangle } from 'lucide-react';
import { Button } from '../ui/Button';
import { addToast } from '../ui/Toast';
import { ContentBrowser, type ContentType } from './ContentBrowser';
import { useFocusStore } from '../../stores/focusStore';
import { useT } from '../../lib/i18n';

/// Strip a trailing version like "_v1.2.3", "-1.0", " v3.2" from a name
function strip_version_suffix(name: string): string {
  return name
    .replace(/[_\-\s]v?\d+(\.\d+)*[a-z]?$/i, '')
    .replace(/[_\-\s]\d+\.\d+\.\d+$/i, '')
    .trim();
}

/// Build a list of progressively-shorter name variants to try in a Modrinth search
function build_name_variants(name: string): string[] {
  const variants: string[] = [];
  if (!name) return variants;
  variants.push(name);
  const stripped = strip_version_suffix(name);
  if (stripped && stripped !== name) variants.push(stripped);
  // First 1-2 words as last-resort
  const words = stripped.split(/[\s_\-()]+/).filter(Boolean);
  if (words.length >= 2) variants.push(words.slice(0, 2).join(' '));
  if (words.length >= 1) variants.push(words[0]);
  return variants;
}

interface ContentItem {
  filename: string;
  name: string;
  version: string;
  provider: string;
  enabled: boolean;
  icon: string | null;
  size?: number;
  slug?: string | null;
  slug_verified?: boolean;
  project_id?: string;
}

interface UpdateInfo {
  name: string;
  oldVersion: string;
  newVersion: string;
  downloadUrl: string;
  filename: string;
}

interface Props {
  instanceName: string;
  contentType: ContentType;
  mcVersion?: string | null;
  loader?: string | null;
  onOpenFolder: () => void;
}

const SUBFOLDER: Record<ContentType, string> = {
  mod: 'mods',
  resourcepack: 'resourcepacks',
  shader: 'shaderpacks',
};

export function ContentManager({ instanceName, contentType, mcVersion, loader, onOpenFolder }: Props) {
  const t = useT();
  const [items, setItems] = useState<ContentItem[]>([]);
  const [itemIcons, setItemIcons] = useState<Record<string, string>>({});
  const [failedIcons, setFailedIcons] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);
  const [search, setSearch] = useState('');
  // Selected by filename (NOT by filtered index) so changing the search
  // query doesn't shift the selection onto the wrong items. `Set<string>`
  // is also stable across `items` array reorders from re-loads.
  const [selectedFilenames, setSelectedFilenames] = useState<Set<string>>(new Set());
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; filename: string } | null>(null);
  const [compatibility, setCompatibility] = useState<Record<string, boolean>>({});
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [updates, setUpdates] = useState<UpdateInfo[] | null>(null);
  const [showBrowser, setShowBrowser] = useState(false);
  // Heavy effects (file watcher, compatibility checks) are skipped while the window
  // is unfocused AND a game is running — this is the "frozen" state. State and UI
  // remain mounted; only the network/CPU work is suspended.
  const isFrozen = useFocusStore((s) => s.isFrozen);

  // Persistent in-memory mirror of the Rust icon cache.
  // Keyed by `modrinth:<project_id>` (packs) or `file:<filename>` (mods / packs without project_id).
  // Hydrated from disk on mount, written to disk when new icons are resolved.
  const iconCacheRef = useRef<Map<string, string>>(new Map());
  const [, setIconCacheVersion] = useState(0);
  const projectNameCache = useRef<Map<string, string>>(new Map());

  const TYPE_LABELS: Record<ContentType, string> = {
    mod: t('content.type_mod'),
    resourcepack: t('content.type_resourcepack'),
    shader: t('content.type_shader'),
  };

  const label = TYPE_LABELS[contentType];
  const subfolder = SUBFOLDER[contentType];

  const cacheIcon = useCallback((key: string, value: string) => {
    if (!key) return;
    if (iconCacheRef.current.get(key) === value) return;
    iconCacheRef.current.set(key, value);
    setItemIcons((prev) => prev[key] === value ? prev : { ...prev, [key]: value });
    setIconCacheVersion((v) => v + 1);
    invoke('cmd_set_icon_cache_entry', { key, value }).catch(() => {});
  }, []);

  const fetchPackIcons = useCallback(async (rawPacks: any[]) => {
    // Use the same source as PackBrowser: Modrinth CDN icons. For packs with project_id,
    // do a direct lookup. For others, search Modrinth by name to find the project.
    // Local pack.png is a fallback if Modrinth icon is missing.
    const projectType = contentType === 'resourcepack' ? 'resourcepack' : contentType === 'shader' ? 'shader' : 'mod';
    const cache = iconCacheRef.current;

    // Build per-pack plans: track which keys are settled and which need a fetch.
    type Plan = { filename: string; name: string; projectId: string | null; modrinthKey: string; localKey: string; hasModrinth: boolean; hasLocal: boolean; resolved: boolean };
    const plans = new Map<string, Plan>();
    for (const p of rawPacks) {
      if (!p.filename) continue;
      let projectId: string | null = p.project_id || null;
      if (!projectId) {
        projectId = projectNameCache.current.get(p.name) || null;
      }
      const modrinthKey = projectId ? `modrinth:${projectId}` : '';
      const localKey = `file:${p.filename}`;
      const hasModrinth = !!modrinthKey && cache.has(modrinthKey);
      const hasLocal = cache.has(localKey);
      plans.set(p.filename, { filename: p.filename, name: p.name, projectId, modrinthKey, localKey, hasModrinth, hasLocal, resolved: false });
    }

    // Step 1: apply cached icons immediately
    for (const plan of plans.values()) {
      if (plan.hasModrinth && plan.modrinthKey) cacheIcon(plan.modrinthKey, cache.get(plan.modrinthKey)!);
      if (plan.hasLocal) cacheIcon(plan.localKey, cache.get(plan.localKey)!);
      if (plan.hasModrinth || plan.hasLocal) plan.resolved = true;
    }

    // Step 2: for packs without a project_id, search Modrinth by name (in parallel)
    const needSearch = Array.from(plans.values()).filter((p) => !p.projectId && !p.resolved);
    await Promise.all(needSearch.map(async (plan) => {
      if (!plan.name) return;
      const variants = build_name_variants(plan.name);
      for (const query of variants) {
        try {
          const res = await invoke<{ hits: Array<{ project_id: string; title?: string; icon_url: string | null }> }>('cmd_search_modrinth', {
            query, projectType, mcVersion: null, loader: null, offset: 0, limit: 5,
          });
          const lower = plan.name.toLowerCase();
          const hit = res.hits.find((h) => h.title?.toLowerCase() === lower)
                   || res.hits.find((h) => h.title?.toLowerCase().startsWith(lower.split(' ')[0]))
                   || res.hits[0];
          if (hit?.project_id) {
            plan.projectId = hit.project_id;
            plan.modrinthKey = `modrinth:${hit.project_id}`;
            projectNameCache.current.set(plan.name, hit.project_id);
            if (cache.has(plan.modrinthKey)) {
              plan.hasModrinth = true;
              cacheIcon(plan.modrinthKey, cache.get(plan.modrinthKey)!);
            }
            return;
          }
        } catch { }
      }
    }));

    // Step 3: fetch Modrinth project info for plans that need it
    const needModrinthFetch = Array.from(plans.values()).filter((p) => p.projectId && p.modrinthKey && !p.hasModrinth && !p.resolved);
    await Promise.all(needModrinthFetch.map(async (plan) => {
      try {
        const project = await invoke<{ icon_url?: string | null }>('cmd_get_modrinth_project', { id: plan.projectId! });
        if (project?.icon_url) {
          plan.hasModrinth = true;
          cacheIcon(plan.modrinthKey, project.icon_url);
        }
      } catch { }
    }));

    // Step 4: fallback to local pack.png for all plans that still lack a Modrinth icon
    // (works for resource packs that include pack.png; shader packs rarely have one but try anyway)
    const needLocal = Array.from(plans.values()).filter((p) => !p.hasModrinth && !p.hasLocal);
    await Promise.all(needLocal.map(async (plan) => {
      try {
        const icon = await invoke<string | null>('cmd_get_pack_icon', { instanceName, packType: subfolder, filename: plan.filename });
        if (icon) {
          plan.hasLocal = true;
          cacheIcon(plan.localKey, icon);
        }
      } catch { }
    }));
  }, [instanceName, subfolder, contentType, cacheIcon]);

  const loadItems = useCallback(async () => {
    setLoading(true);
    try {
      if (contentType === 'mod') {
        const raw = await invoke<any[]>('cmd_get_mod_metadata', { instanceName });
        const mapped = raw.map((m) => ({
          filename: m.filename,
          name: m.name,
          version: m.version || '',
          provider: m.provider || '',
          enabled: m.enabled,
          icon: m.icon,
          slug: m.slug,
          slug_verified: m.slug_verified,
          project_id: m.slug_verified ? m.slug : undefined,
        }));
        setItems(mapped);
        const cache = iconCacheRef.current;
        for (const m of raw) {
          const key = `file:${m.filename}`;
          const cached = cache.get(key);
          if (cached) {
            // Already cached - apply immediately
            cacheIcon(key, cached);
            continue;
          }
          // Not in cache: only fetch from jar if no remote icon
          if (m.icon && m.icon.startsWith('data:')) continue;
          try {
            const icon = await invoke<string | null>('cmd_get_mod_icon', { instanceName, filename: m.filename });
            if (icon) cacheIcon(key, icon);
          } catch { }
        }
      } else {
        const raw = await invoke<any[]>('cmd_list_packs', { instanceName, packType: subfolder });
        setItems(raw.map((p) => ({
          filename: p.filename,
          name: p.name,
          version: p.version || '',
          provider: p.provider || '',
          project_id: p.project_id || '',
          slug: p.project_id || undefined,
          enabled: !p.filename.endsWith('.disabled'),
          icon: null,
          size: p.file_size,
        })));
        // Pack icons are resolved by fetchPackIcons (uses persistent cache internally)
        fetchPackIcons(raw);
      }
    } catch (e: any) {
      addToast(t('manager.load_error', { label, error: e.toString() }), 'error');
    }
    setLoading(false);
  }, [instanceName, contentType, subfolder, fetchPackIcons, label, cacheIcon]);

  useEffect(() => {
    loadItems();
    setSelectedFilenames(new Set());
    setSearch('');
    setCompatibility({});
    setFailedIcons(new Set());
    checkedRef.current = new Set();
  }, [loadItems]);

  // Hydrate the icon cache from disk on mount and seed itemIcons for instant display.
  // Runs once on app start; cache is then kept in sync by `cacheIcon` calls.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const cache = await invoke<Record<string, string>>('cmd_get_icon_cache');
        if (cancelled) return;
        const map = new Map(Object.entries(cache));
        iconCacheRef.current = map;
        const seeded: Record<string, string> = {};
        for (const [k, v] of map) seeded[k] = v;
        setItemIcons(seeded);
        setIconCacheVersion((v) => v + 1);
      } catch { }
    })();
    return () => { cancelled = true; };
  }, []);

  // Watch the instance directory for external file changes; reload list on event
  useEffect(() => {
    let unlistenFn: (() => void) | undefined;
    const setup = async () => {
      try {
        // Start watching this instance's content dirs (mods/resourcepacks/shaderpacks)
        await invoke('cmd_watch_instance', { instanceName });
        // Listen for change events
        const { listen } = await import('@tauri-apps/api/event');
        const unlisten = await listen<{ instance: string; subfolder: string }>('instance_dir_changed', (e) => {
          // Only react if the change is in the subfolder we currently display
          if (e.payload.instance !== instanceName) return;
          if (e.payload.subfolder !== subfolder) return;
          if (showBrowser || checkingUpdates || updates || contextMenu) return;
          // While the launcher is frozen (unfocused during a game), defer the reload
          // — a single loadItems will be triggered by the [isFrozen] effect below
          // when focus returns. This avoids running fetches in the background.
          if (isFrozen) return;
          loadItems();
        });
        unlistenFn = unlisten;
      } catch { }
    };
    setup();
    return () => {
      if (unlistenFn) unlistenFn();
      invoke('cmd_unwatch_instance').catch(() => {});
    };
  }, [instanceName, subfolder, loadItems, showBrowser, checkingUpdates, updates, contextMenu, isFrozen]);

  const checkedRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    // Compatibility checking fires network requests every 200ms per mod.
    // While the launcher is frozen, skip this entirely — the check resumes
    // automatically when focus is restored (we just won't recompute for already-checked items).
    if (isFrozen) return;
    if (loading || contentType !== 'mod') return;
    const visible = items.filter((p) => p.name.toLowerCase().includes(search.toLowerCase()) || p.filename.toLowerCase().includes(search.toLowerCase()));
    const toCheck = visible.filter((it) => it.slug && it.slug_verified && !checkedRef.current.has(it.filename));
    if (toCheck.length === 0) return;
    let cancelled = false;
    (async () => {
      for (const item of toCheck) {
        if (cancelled) break;
        checkedRef.current.add(item.filename);
        try {
          // Strict query: instance's MC version + loader.
          let vers = await invoke<any[]>('cmd_get_modrinth_versions', {
            projectId: item.slug,
            mcVersion: mcVersion ?? null,
            loader: loader ?? null,
          });
          if (cancelled) break;
          // Fallback: if the strict query returned nothing, the mod may still
          // be compatible — the Modrinth project just lacks a version tagged
          // for the exact MC version (e.g. instance is on 1.21.11 and the
          // mod's tagged versions only cover 1.21.1). Re-query with just the
          // loader; if the mod has *any* Fabric/Forge/… version we treat it
          // as compatible, since the loader is the harder constraint.
          if (vers.length === 0 && loader && loader !== 'Vanilla') {
            vers = await invoke<any[]>('cmd_get_modrinth_versions', {
              projectId: item.slug,
              mcVersion: null,
              loader,
            });
            if (cancelled) break;
          }
          if (vers.length === 0) {
            setCompatibility((prev) => ({ ...prev, [item.filename]: true }));
          }
        } catch {
          if (cancelled) break;
        }
        await new Promise((r) => setTimeout(r, 200));
      }
    })();
    return () => { cancelled = true; };
  }, [items, loading, contentType, mcVersion, loader, search, isFrozen]);

  // When the launcher thaws (focus returns), the file watcher may have queued
  // changes that we ignored. Refresh the list once to pick up any external edits.
  const wasFrozen = useRef(false);
  useEffect(() => {
    if (isFrozen) { wasFrozen.current = true; return; }
    if (wasFrozen.current) {
      wasFrozen.current = false;
      // Single, cheap sync — no heavy work; just the list of items.
      loadItems();
    }
  }, [isFrozen, loadItems]);

  useEffect(() => {
    const handler = () => setContextMenu(null);
    window.addEventListener('click', handler);
    return () => window.removeEventListener('click', handler);
  }, []);

  const getSelectedItems = (): ContentItem[] => {
    if (selectedFilenames.size === 0) return [];
    const byName = new Map(items.map((it) => [it.filename, it]));
    return Array.from(selectedFilenames)
      .map((fn) => byName.get(fn))
      .filter((x): x is ContentItem => !!x);
  };

  const toggleEnabled = async (item: ContentItem) => {
    try {
      const dir = await invoke<string>('cmd_get_instance_dir', { instanceName });
      const itemDir = `${dir}/${subfolder}`;
      const newEnabled = !item.enabled;
      const newName = newEnabled ? item.filename.replace(/\.disabled$/, '') : `${item.filename}.disabled`;
      await invoke('cmd_rename_file', { from: `${itemDir}/${item.filename}`, to: `${itemDir}/${newName}` });
      // Rename sidecar too
      try {
        await invoke('cmd_rename_file', { from: `${itemDir}/${item.filename}.voidlauncher.json`, to: `${itemDir}/${newName}.voidlauncher.json` });
      } catch { }
      setItems((prev) => prev.map((it) => it.filename === item.filename ? { ...it, filename: newName, enabled: newEnabled } : it));
      if (itemIcons[item.filename]) {
        setItemIcons((prev) => {
          const copy = { ...prev };
          copy[newName] = copy[item.filename];
          delete copy[item.filename];
          return copy;
        });
      }
    } catch (e: any) { addToast(t('manager.toggle_error', { error: e.toString() }), 'error'); }
  };

  const toggleSelectedEnabled = async () => {
    const selected = getSelectedItems();
    if (selected.length === 0) return;
    for (const item of selected) {
      await toggleEnabled(item);
    }
    setSelectedFilenames(new Set());
  };

  const removeSelected = async () => {
    const selected = getSelectedItems();
    if (selected.length === 0) return;
    for (const item of selected) {
      await removeSingleItem(item.filename);
    }
    setSelectedFilenames(new Set());
    loadItems();
  };

  const removeSingleItem = async (filename: string) => {
    try {
      const dir = await invoke<string>('cmd_get_instance_dir', { instanceName });
      const itemPath = `${dir}/${subfolder}/${filename}`;
      await invoke('cmd_delete_file', { path: itemPath });
      // Also remove sidecar metadata file (pack tracking)
      const sidecarPath = `${dir}/${subfolder}/${filename}.voidlauncher.json`;
      try { await invoke('cmd_delete_file', { path: sidecarPath }); } catch { }
    } catch (e: any) { addToast(t('manager.remove_error', { name: filename, error: e.toString() }), 'error'); }
  };

  const handleAddLocalFile = async () => {
    const ext = contentType === 'mod' ? 'jar' : 'zip';
    const selected = await openFileDialog({
      title: t('manager.add_file_dialog', { label }),
      filters: [{ name: label, extensions: [ext] }],
      multiple: true,
    });
    if (!selected || selected.length === 0) return;
    const files = Array.isArray(selected) ? selected : [selected];
    for (const filePath of files) {
      const filename = filePath.split(/[/\\]/).pop() || '';
      try {
        await invoke('cmd_install_mod', { instanceName, downloadUrl: `file://${filePath}`, fileName: filename, provider: 'local' });
      } catch (e: any) { addToast(t('manager.add_error', { name: filename, error: e.toString() }), 'error'); }
    }
    loadItems();
  };

  const handleCheckUpdates = async () => {
    const toCheck = selectedFilenames.size > 0
      ? getSelectedItems().filter((m) => (m.provider === 'Modrinth' || m.provider === 'CurseForge') && (m.project_id || m.slug))
      : items.filter((m) => m.enabled && (m.provider === 'Modrinth' || m.provider === 'CurseForge') && (m.project_id || m.slug));

    if (toCheck.length === 0) { addToast(t('manager.no_updates'), 'info'); return; }

    setCheckingUpdates(true);
    addToast(t('manager.checking_updates', { count: toCheck.length.toString() }), 'info');
    const found: UpdateInfo[] = [];

    for (const m of toCheck) {
      const pid = m.project_id || m.slug || '';
      try {
        const vers = await invoke<any[]>('cmd_get_modrinth_versions', { projectId: pid, mcVersion: mcVersion ?? null, loader: contentType === 'mod' ? (loader ?? null) : null });
        if (vers.length > 0) {
          const latest = vers[0];
          const latestVersion = latest.version_number || latest.name || '';
          // For packs (no version) always consider as update; for mods only if version differs
          const isUpdate = contentType !== 'mod' || (latestVersion && latestVersion !== m.version);
          if (isUpdate) {
            const file = latest.files?.find((f: any) => f.primary) || latest.files?.[0];
            if (file) {
              found.push({
                name: m.name,
                oldVersion: m.version || '(installed)',
                newVersion: latestVersion,
                downloadUrl: file.url,
                filename: file.filename,
              });
            }
          }
        }
      } catch { }
    }

    setCheckingUpdates(false);
    if (found.length > 0) {
      setUpdates(found);
    } else {
      addToast(t('common.up_to_date'), 'success');
    }
  };

  const handleApplyUpdate = async (update: UpdateInfo) => {
    try {
      await invoke('cmd_download_to_folder', { instanceName, downloadUrl: update.downloadUrl, fileName: update.filename, subfolder, projectId: null, projectName: update.name || null, versionId: null, versionNumber: null, provider: 'modrinth' });
      addToast(t('manager.updated_toast', { name: update.name, version: update.newVersion }), 'success');
      setUpdates((prev) => prev ? prev.filter((u) => u.name !== update.name) : null);
      loadItems();
    } catch (e: any) { addToast(t('manager.update_error', { name: update.name, error: e.toString() }), 'error'); }
  };

  const handleApplyAllUpdates = async () => {
    if (!updates) return;
    for (const u of updates) {
      await handleApplyUpdate(u);
    }
    setUpdates(null);
  };

  const getItemIcon = (item: ContentItem): string | null => {
    if (item.icon && item.icon.startsWith('data:')) return item.icon;
    // Look up the persistent icon cache: prefer project_id key, fall back to filename
    if (item.project_id) {
      const v = itemIcons[`modrinth:${item.project_id}`] ?? iconCacheRef.current.get(`modrinth:${item.project_id}`);
      if (v) return v;
    }
    const fileKey = `file:${item.filename}`;
    const v2 = itemIcons[fileKey] ?? iconCacheRef.current.get(fileKey);
    if (v2) return v2;
    if (itemIcons[item.filename]) return itemIcons[item.filename];
    if (item.icon && !item.icon.startsWith('data:') && item.icon.includes('/')) {
      return `https://cdn.modrinth.com/data/${item.icon}`;
    }
    return null;
  };

  const filtered = items.filter((p) => p.name.toLowerCase().includes(search.toLowerCase()) || p.filename.toLowerCase().includes(search.toLowerCase()));

  const handleRowClick = (filename: string, e: React.MouseEvent) => {
    if (e.ctrlKey || e.metaKey) {
      setSelectedFilenames((prev) => {
        const next = new Set(prev);
        if (next.has(filename)) next.delete(filename);
        else next.add(filename);
        return next;
      });
    } else {
      setSelectedFilenames((prev) => {
        if (prev.size === 1 && prev.has(filename)) return new Set();
        return new Set([filename]);
      });
    }
  };

  const selectedItems = getSelectedItems();
  const hasSelection = selectedFilenames.size > 0;

  if (showBrowser) {
    // Normalize: lowercase + underscore→dash so fabric mod ids (e.g. "My_Cool_Mod")
    // match Modrinth slugs ("my-cool-mod"). Both original and normalized are stored
    // so the set works either way.
    const normalizeSlug = (s: string) => s.toLowerCase().replace(/_/g, '-');
    // For name matching, also treat dashes as spaces so a filename-derived name
    // like "3d-skin-layers" matches the Modrinth title "3D Skin Layers".
    const normalizeName = (s: string) => s.toLowerCase().replace(/[-_]+/g, ' ').replace(/\s+/g, ' ').trim();
    const installedIds = new Set(
      items.flatMap((it) => {
        const ids: string[] = [];
        if (it.slug) {
          const n = normalizeSlug(it.slug);
          ids.push(it.slug);
          if (n !== it.slug) ids.push(n);
        }
        if (it.name) {
          ids.push(`name:${normalizeName(it.name)}`);
          // Also add narrower variants: just slug-normalized name helps when
          // Modrinth slug and item slug happen to match space-for-dash.
          const slugForm = normalizeSlug(it.name);
          if (slugForm !== normalizeName(it.name)) ids.push(`name:${slugForm}`);
        }
        return ids;
      })
    );
    return (
      <ContentBrowser
        instanceName={instanceName}
        contentType={contentType}
        mcVersion={mcVersion}
        loader={loader}
        onClose={() => setShowBrowser(false)}
        onInstalled={() => { loadItems(); }}
        installedProjectIds={installedIds}
      />
    );
  }

  return (
    <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>
      {/* Main table */}
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
        {/* Header */}
        <div style={{ padding: 'var(--space-md) var(--space-2xl)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)', borderBottom: '1px solid var(--surface-border)', flexShrink: 0 }}>
          <h2 style={{ fontSize: 'var(--font-size-lg)', fontWeight: 600, margin: 0, flex: 1 }}>
            {label} <span style={{ fontWeight: 400, color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>{t('packs.installed_count', { n: items.length.toString() })}</span>
          </h2>
        </div>

        {/* Table header */}
        <div style={{
                    display: 'grid', gridTemplateColumns: '28px 36px 1fr 1fr 80px 28px',
          padding: '6px var(--space-2xl)', borderBottom: '1px solid var(--surface-border)',
          fontSize: 'var(--font-size-xs)', fontWeight: 600, color: 'var(--text-tertiary)',
          background: 'var(--surface-elevated)', flexShrink: 0,
        }}>
          <div style={{ textAlign: 'center' }}>{t('manager.column_on')}</div>
          <div></div>
          <div>{t('manager.column_name')}</div>
          <div>{contentType === 'mod' ? t('manager.column_version') : t('manager.column_size')}</div>
          <div>{contentType === 'mod' ? t('manager.column_provider') : ''}</div>
          <div></div>
        </div>

        {/* Table body */}
        <div style={{ flex: 1, overflowY: 'auto' }}>
          {loading ? (
            <div style={{ padding: 'var(--space-md) var(--space-2xl)' }}>
              {[1, 2, 3, 4, 5].map((i) => (
                <div key={i} style={{ display: 'grid', gridTemplateColumns: '28px 36px 1fr 1fr 80px 28px', padding: '7px var(--space-2xl)', alignItems: 'center', borderBottom: '1px solid var(--surface-border)' }}>
                  <div style={{ width: 16, height: 16, borderRadius: 3, background: 'var(--surface-glass)', justifySelf: 'center' }} />
                  <div style={{ width: 28, height: 28, borderRadius: 4, background: 'var(--surface-glass)', justifySelf: 'center' }} />
                  <div style={{ width: '60%', height: 14, borderRadius: 4, background: 'var(--surface-glass)' }} />
                  <div style={{ width: 60, height: 14, borderRadius: 4, background: 'var(--surface-glass)' }} />
                  <div style={{ width: 50, height: 14, borderRadius: 4, background: 'var(--surface-glass)' }} />
                  <div></div>
                </div>
              ))}
            </div>
          ) : filtered.length === 0 ? (
            <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>
              <Package size={32} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
              <div>{items.length === 0 ? t('manager.empty', { label: label.toLowerCase() }) : t('manager.empty_search')}</div>
            </div>
          ) : (
            filtered.map((item, idx) => {
              const isSelected = selectedFilenames.has(item.filename);
              const iconSrc = getItemIcon(item);
              return (
                <div key={item.filename}
                  onClick={(e) => handleRowClick(item.filename, e)}
                  onContextMenu={(e) => { e.preventDefault(); setContextMenu({ x: e.clientX, y: e.clientY, filename: item.filename }); }}
                  style={{
          display: 'grid', gridTemplateColumns: '28px 36px 1fr 1fr 80px 28px',
                    padding: '4px var(--space-2xl)', alignItems: 'center', cursor: 'pointer',
                    borderBottom: '1px solid var(--surface-border)',
                    background: isSelected ? 'var(--primary-dim)' : idx % 2 === 0 ? 'transparent' : 'hsla(0, 0%, 100%, 0.02)',
                    opacity: item.enabled ? 1 : 0.45,
                    transition: 'background 0.12s',
                    minHeight: 36,
                  }}
                >
                  <div style={{ display: 'flex', justifyContent: 'center' }}>
                    <div onClick={(e) => { e.stopPropagation(); toggleEnabled(item); }}
                      style={{ width: 16, height: 16, borderRadius: 3, cursor: 'pointer', border: '1.5px solid ' + (!item.enabled ? 'var(--text-tertiary)' : 'var(--primary)'), background: !item.enabled ? 'transparent' : 'var(--primary)', display: 'flex', alignItems: 'center', justifyContent: 'center', transition: 'all 0.15s' }}>
                      {item.enabled && <Check size={10} color="white" strokeWidth={3} />}
                    </div>
                  </div>
                  <div style={{ display: 'flex', justifyContent: 'center' }}>
                    {iconSrc && !failedIcons.has(item.filename) ? (
                      <img key={item.filename} src={iconSrc} alt="" style={{ width: 28, height: 28, borderRadius: 4, objectFit: 'cover' }}
                        onError={() => setFailedIcons((prev) => new Set(prev).add(item.filename))} />
                    ) : (
                      <div style={{ width: 28, height: 28, borderRadius: 4, background: 'var(--surface-glass)', display: 'flex', alignItems: 'center', justifyContent: 'center', fontSize: 10, color: 'var(--text-tertiary)' }}>
                        {item.name.charAt(0)}
                      </div>
                    )}
                  </div>
                  <div style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 'var(--font-size-sm)', fontWeight: 500, paddingRight: 8 }}>
                    {item.name}
                  </div>
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {contentType === 'mod' ? item.version : formatSize(item.size ?? 0)}
                  </div>
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                    {contentType === 'mod' ? item.provider : ''}
                  </div>
                  <div>
                    {contentType === 'mod' && compatibility[item.filename] && (() => {
                      // Build a localized warning string. The yellow
                      // "this mod may be incompatible" tooltip used to be
                      // hardcoded English, which broke the Russian UI.
                      const versionPart = mcVersion ? `MC ${mcVersion}` : '';
                      const loaderPart = loader && loader !== 'Vanilla' ? loader : '';
                      const target = [versionPart, loaderPart].filter(Boolean).join(' / ');
                      const title =
                        !item.version && !item.provider
                          ? t('manager.compat_unknown')
                          : mcVersion && !item.version
                            ? t('manager.compat_maybe', { version: mcVersion })
                            : t('manager.compat_no', { target });
                      return (
                        <div style={{ position: 'relative', display: 'inline-flex', cursor: 'help' }} title={title}>
                          <div style={{ width: 20, height: 20, borderRadius: '50%', border: '2px solid var(--warning)', display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--warning)' }}>
                            <AlertTriangle size={12} />
                          </div>
                        </div>
                      );
                    })()}
                  </div>
                </div>
              );
            })
          )}
        </div>

        {/* Bottom bar */}
        <div style={{ padding: '6px var(--space-2xl)', borderTop: '1px solid var(--surface-border)', display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', flexShrink: 0, background: 'var(--surface-elevated)' }}>
          <div style={{ position: 'relative', flex: 1, maxWidth: 300 }}>
            <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--text-tertiary)' }} />
            <input className="input" type="text" placeholder={t('manager.search_placeholder', { label: label.toLowerCase() })} value={search} onChange={(e) => setSearch(e.target.value)} style={{ paddingLeft: 32, fontSize: 'var(--font-size-sm)' }} />
          </div>
          {hasSelection && (
            <span style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{t('common.items_selected', { n: selectedFilenames.size.toString() })}</span>
          )}
          <div style={{ flex: 1 }} />
          <Button size="sm" variant="ghost" onClick={loadItems} disabled={loading} title={t('manager.auto_refresh_title')} style={{ minWidth: 80 }}>
            {loading ? <RefreshCw size={14} style={{ animation: 'spin 1s linear infinite' }} /> : <RefreshCw size={14} />}
            {loading ? t('common.refresh') : t('common.refresh')}
          </Button>
        </div>

      </div>

      {/* Right actions panel */}
      <div style={{ width: 200, flexShrink: 0, borderLeft: '1px solid var(--surface-border)', padding: 'var(--space-md)', display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)', background: 'var(--surface-elevated)' }}>
        <Button size="sm" variant="secondary" onClick={() => setShowBrowser(true)} style={{ width: '100%' }}>
          <Download size={14} /> {t('common.download')} {label}
        </Button>
        <Button size="sm" variant="ghost" onClick={handleAddLocalFile} style={{ width: '100%' }}>
          <Package size={14} /> {t('manager.add_file_btn')}
        </Button>
        <div style={{ height: 1, background: 'var(--surface-border)', margin: '4px 0' }} />
        <Button size="sm" variant="ghost" onClick={handleCheckUpdates} loading={checkingUpdates} disabled={checkingUpdates} style={{ width: '100%' }}>
          {!checkingUpdates && <RefreshCw size={14} />}
          {t('common.check_updates')}
        </Button>
        {hasSelection && (
          <>
            <Button size="sm" variant="ghost" onClick={toggleSelectedEnabled} style={{ width: '100%' }}>
              {selectedItems.every((m) => m.enabled) ? t('common.disable') : t('common.enable')}{selectedFilenames.size > 1 ? ` (${selectedFilenames.size})` : ''}
            </Button>
            <Button size="sm" variant="ghost" onClick={removeSelected} style={{ width: '100%', color: 'var(--color-danger)' }}>
              <Trash2 size={14} /> {t('common.remove')}{selectedFilenames.size > 1 ? ` (${selectedFilenames.size})` : ''}
            </Button>
          </>
        )}
        {hasSelection && selectedItems.length === 1 && (
          <>
            <div style={{ height: 1, background: 'var(--surface-border)', margin: '4px 0' }} />
            <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', lineHeight: 1.5 }}>
              <div><strong>{t('manager.detail_file')}</strong> {selectedItems[0].filename}</div>
              {selectedItems[0].version && <div><strong>{t('manager.detail_version')}</strong> {selectedItems[0].version}</div>}
              {selectedItems[0].provider && <div><strong>{t('manager.detail_provider')}</strong> {selectedItems[0].provider}</div>}
            </div>
          </>
        )}
        <div style={{ flex: 1 }} />
        <div style={{ height: 1, background: 'var(--surface-border)', margin: '4px 0' }} />
        <Button size="sm" variant="ghost" onClick={onOpenFolder} style={{ width: '100%' }}>
          <FolderOpen size={14} /> {t('common.open_folder')}
        </Button>
      </div>

      {/* Context menu */}
      {contextMenu && (() => {
        const ctxItem = items.find((it) => it.filename === contextMenu.filename);
        return (
        <div style={{ position: 'fixed', left: contextMenu.x, top: contextMenu.y, zIndex: 9999, background: 'var(--surface-elevated)', border: '1px solid var(--surface-border)', borderRadius: 'var(--radius-md)', padding: 4, minWidth: 160, boxShadow: '0 8px 24px rgba(0,0,0,0.4)' }}>
          <button style={{ display: 'block', width: '100%', padding: '6px 12px', border: 'none', background: 'none', color: 'var(--text-primary)', fontSize: 'var(--font-size-sm)', textAlign: 'left', cursor: 'pointer', borderRadius: 'var(--radius-sm)' }}
            onMouseEnter={(e) => e.currentTarget.style.background = 'var(--bg-secondary)'}
            onMouseLeave={(e) => e.currentTarget.style.background = 'none'}
            onClick={() => { if (ctxItem) toggleEnabled(ctxItem); setContextMenu(null); }}
            disabled={!ctxItem}>
            {ctxItem?.enabled ? t('common.disable') : t('common.enable')}
          </button>
          <button style={{ display: 'block', width: '100%', padding: '6px 12px', border: 'none', background: 'none', color: 'var(--color-danger)', fontSize: 'var(--font-size-sm)', textAlign: 'left', cursor: 'pointer', borderRadius: 'var(--radius-sm)' }}
            onMouseEnter={(e) => e.currentTarget.style.background = 'var(--bg-secondary)'}
            onMouseLeave={(e) => e.currentTarget.style.background = 'none'}
            onClick={() => { if (ctxItem) removeSingleItem(ctxItem.filename); setContextMenu(null); }}
            disabled={!ctxItem}>
            <Trash2 size={12} style={{ marginRight: 6 }} /> {t('common.remove')}
          </button>
        </div>
        );
      })()}

      {/* Update Dialog */}
      {updates && (
        <div style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 9999 }}>
          <div className="glass-card" style={{ padding: 'var(--space-xl)', maxWidth: 600, width: '90%', maxHeight: '70vh', display: 'flex', flexDirection: 'column' }}>
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 'var(--space-md)' }}>
              <h3 style={{ margin: 0, fontSize: 'var(--font-size-lg)', fontWeight: 700 }}>{t('manager.updates_title', { count: updates.length.toString() })}</h3>
              <X size={16} style={{ cursor: 'pointer', color: 'var(--text-tertiary)' }} onClick={() => setUpdates(null)} />
            </div>
            <div style={{ flex: 1, overflowY: 'auto', marginBottom: 'var(--space-md)' }}>
              {updates.map((u, i) => (
                <div key={i} style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', padding: '8px 12px', borderRadius: 'var(--radius-md)', background: 'var(--bg-secondary)', marginBottom: 4 }}>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontWeight: 500, fontSize: 'var(--font-size-sm)' }}>{u.name}</div>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: 2 }}>
                      <span style={{ textDecoration: 'line-through', opacity: 0.6 }}>{u.oldVersion}</span>
                      <ArrowRight size={10} color="var(--success)" />
                      <span style={{ color: 'var(--success)', fontWeight: 500 }}>{u.newVersion}</span>
                    </div>
                  </div>
                  <Button size="sm" onClick={() => handleApplyUpdate(u)} style={{ flexShrink: 0 }}>
                    {t('common.update')}
                  </Button>
                </div>
              ))}
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)' }}>
              <Button variant="ghost" onClick={() => setUpdates(null)}>{t('common.cancel')}</Button>
              <Button onClick={handleApplyAllUpdates}>
                {t('manager.update_all', { count: updates.length.toString() })}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
