import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { marked } from 'marked';
import DOMPurify from 'dompurify';
import { Search, ArrowLeft, Package, ArrowUpDown, Star, Calendar, Loader2, X, Check, Download } from 'lucide-react';
import { Button } from '../ui/Button';
import { addToast } from '../ui/Toast';
import { useT } from '../../lib/i18n';
import { openUrl } from '@tauri-apps/plugin-opener';

interface Hit {
  project_id: string;
  title: string;
  description: string;
  icon_url: string | null;
  project_type: string;
  downloads: number;
  slug: string;
  author?: string;
}

export interface SelectedItem {
  name: string;
  iconUrl?: string;
  downloadUrl: string;
  filename: string;
  source: string;
  projectId: string;
  versionName?: string;
  isDependency?: boolean;
  parentMod?: string;
}

export type ContentType = 'mod' | 'resourcepack' | 'shader';

interface Props {
  instanceName: string;
  contentType: ContentType;
  mcVersion?: string | null;
  loader?: string | null;
  onClose: () => void;
  onInstalled: () => void;
  /** Project IDs of mods/packs already installed on disk (verified slugs only). */
  installedProjectIds?: Set<string>;
}

type SortMode = 'downloads' | 'newest' | 'oldest';

const SUBFOLDER: Record<ContentType, string> = {
  mod: 'mods',
  resourcepack: 'resourcepacks',
  shader: 'shaderpacks',
};

const normalizeSlug = (s: string) => s.toLowerCase().replace(/_/g, '-');
const normalizeName = (s: string) => s.toLowerCase().replace(/[-_]+/g, ' ').replace(/\s+/g, ' ').trim();

export function ContentBrowser({ instanceName, contentType, mcVersion, loader, onClose, onInstalled, installedProjectIds }: Props) {
  const t = useT();
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<Hit[]>([]);
  const [loading, setLoading] = useState(false);
  const [popular, setPopular] = useState<Hit[]>([]);
  const [selected, setSelected] = useState<Hit | null>(null);
  const [modDetail, setModDetail] = useState<any>(null);
  const [loadingDetail, setLoadingDetail] = useState(false);
  const [versions, setVersions] = useState<any[]>([]);
  const [sortMode, setSortMode] = useState<SortMode>('downloads');
  const [selectedItems, setSelectedItems] = useState<SelectedItem[]>([]);
  const [addingId, setAddingId] = useState<string | null>(null);
  const [subView, setSubView] = useState<'search' | 'confirm'>('search');
  const [installing, setInstalling] = useState(false);
  const [downloadProgress, setDownloadProgress] = useState(0);
  const [popularOffset, setPopularOffset] = useState(0);
  const [resultsOffset, setResultsOffset] = useState(0);
  const [hasMorePopular, setHasMorePopular] = useState(true);
  const [hasMoreResults, setHasMoreResults] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [versionLimit, setVersionLimit] = useState(20);
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  const versionSentinelRef = useRef<HTMLDivElement | null>(null);
  const PAGE_SIZE = 50;

  const TYPE_LABELS: Record<ContentType, string> = {
    mod: t('content.type_mod'),
    resourcepack: t('content.type_resourcepack'),
    shader: t('content.type_shader'),
  };

  const label = TYPE_LABELS[contentType];
  const subfolder = SUBFOLDER[contentType];
  const versionLoader = contentType === 'mod' ? (loader ?? null) : null;

  const loadPopular = useCallback(async () => {
    setLoading(true);
    setPopular([]);
    setPopularOffset(0);
    setHasMorePopular(true);
    try {
      const res = await invoke<any>('cmd_popular_modrinth', { projectType: contentType, mcVersion: mcVersion ?? null, loader: loader ?? null, offset: 0, limit: PAGE_SIZE });
      const hits: Hit[] = res.hits || [];
      setPopular(hits);
      setPopularOffset(hits.length);
      setHasMorePopular(hits.length >= PAGE_SIZE);
    } catch { }
    setLoading(false);
  }, [contentType, mcVersion, loader]);

  const popularOffsetRef = useRef(0);
  popularOffsetRef.current = popularOffset;

  const loadMorePopular = useCallback(async () => {
    if (!hasMorePopular || loadingMore) return;
    setLoadingMore(true);
    const offset = popularOffsetRef.current;
    try {
      const res = await invoke<any>('cmd_popular_modrinth', { projectType: contentType, mcVersion: mcVersion ?? null, loader: loader ?? null, offset, limit: PAGE_SIZE });
      const hits: Hit[] = res.hits || [];
      setPopular((prev) => {
        const seen = new Set(prev.map((h) => h.project_id));
        const fresh = hits.filter((h) => !seen.has(h.project_id));
        return [...prev, ...fresh];
      });
      setPopularOffset(offset + hits.length);
      setHasMorePopular(hits.length >= PAGE_SIZE);
    } catch { }
    setLoadingMore(false);
  }, [contentType, mcVersion, loader, hasMorePopular, loadingMore]);

  useEffect(() => { loadPopular(); }, [loadPopular]);

  const handleSearch = async () => {
    if (!query.trim()) { loadPopular(); return; }
    setLoading(true);
    setResults([]);
    setResultsOffset(0);
    setHasMoreResults(true);
    setSelected(null);
    try {
      const res = await invoke<any>('cmd_search_modrinth', { query, projectType: contentType, mcVersion: mcVersion ?? null, loader: loader ?? null, offset: 0, limit: PAGE_SIZE });
      const hits: Hit[] = res.hits || [];
      setResults(hits);
      setResultsOffset(hits.length);
      setHasMoreResults(hits.length >= PAGE_SIZE);
    } catch (e: any) { addToast(t('content.search_error', { error: e.toString() }), 'error'); }
    setLoading(false);
  };

  const resultsOffsetRef = useRef(0);
  resultsOffsetRef.current = resultsOffset;

  const loadMoreResults = useCallback(async () => {
    if (!hasMoreResults || loadingMore || !query.trim()) return;
    setLoadingMore(true);
    const offset = resultsOffsetRef.current;
    try {
      const res = await invoke<any>('cmd_search_modrinth', { query, projectType: contentType, mcVersion: mcVersion ?? null, loader: loader ?? null, offset, limit: PAGE_SIZE });
      const hits: Hit[] = res.hits || [];
      setResults((prev) => {
        const seen = new Set(prev.map((h) => h.project_id));
        const fresh = hits.filter((h) => !seen.has(h.project_id));
        return [...prev, ...fresh];
      });
      setResultsOffset(offset + hits.length);
      setHasMoreResults(hits.length >= PAGE_SIZE);
    } catch { }
    setLoadingMore(false);
  }, [contentType, mcVersion, loader, query, hasMoreResults, loadingMore]);

  useEffect(() => {
    const el = sentinelRef.current;
    if (!el) return;
    const obs = new IntersectionObserver((entries) => {
      const entry = entries[0];
      if (!entry.isIntersecting) return;
      if (loading || loadingMore) return;
      if (query.trim().length > 0) {
        loadMoreResults();
      } else {
        loadMorePopular();
      }
    }, { rootMargin: '200px' });
    obs.observe(el);
    return () => obs.disconnect();
  }, [loading, loadingMore, query, loadMoreResults, loadMorePopular]);

  // Version infinite scroll: load 20 more when reaching the bottom of the version list
  useEffect(() => {
    const el = versionSentinelRef.current;
    if (!el || !selected || versions.length <= versionLimit) return;
    const obs = new IntersectionObserver((entries) => {
      if (entries[0].isIntersecting) {
        setVersionLimit((prev) => Math.min(prev + 20, versions.length));
      }
    }, { rootMargin: '100px' });
    obs.observe(el);
    return () => obs.disconnect();
  }, [selected, versions, versionLimit]);

  const handleSelect = async (hit: Hit) => {
    setSelected(hit);
    setLoadingDetail(true);
    setModDetail(null);
    setVersions([]);
    setVersionLimit(20);
    try {
      const [detail, vers] = await Promise.all([
        invoke<any>('cmd_get_modrinth_project', { id: hit.project_id }),
        invoke<any[]>('cmd_get_modrinth_versions', { projectId: hit.project_id, mcVersion: mcVersion ?? null, loader: versionLoader }),
      ]);
      setModDetail(detail);
      setVersions(vers);
    } catch (e: any) { addToast(t('content.detail_error', { error: e.toString() }), 'error'); }
    setLoadingDetail(false);
  };

  const isAdded = (id: string, slug?: string, title?: string) =>
    selectedItems.some((m) => m.projectId === id)
    || (installedProjectIds?.has(normalizeSlug(id)) ?? false)
    || (slug ? (installedProjectIds?.has(normalizeSlug(slug)) ?? false) : false)
    || (title ? (installedProjectIds?.has(`name:${normalizeName(title)}`) ?? false) : false);
  const isInstalled = (id: string, slug?: string, title?: string) =>
    (installedProjectIds?.has(normalizeSlug(id)) ?? false)
    || (slug ? (installedProjectIds?.has(normalizeSlug(slug)) ?? false) : false)
    || (title ? (installedProjectIds?.has(`name:${normalizeName(title)}`) ?? false) : false);

  const addLatest = async (hit: Hit) => {
    if (selectedItems.some((m) => m.projectId === hit.project_id) || isInstalled(hit.project_id, hit.slug, hit.title)) {
      return;
    }
    setAddingId(hit.project_id);
    try {
      const vers = await invoke<any[]>('cmd_get_modrinth_versions', { projectId: hit.project_id, mcVersion: mcVersion ?? null, loader: versionLoader });
      if (vers.length === 0) {
        addToast(t('content.no_compatible_version'), 'warning');
        setAddingId(null);
        return;
      }
      const v = vers[0];
      const file = v.files?.find((f: any) => f.primary) || v.files?.[0];
      if (file) {
        const item: SelectedItem = {
          name: hit.title,
          iconUrl: hit.icon_url ?? undefined,
          downloadUrl: file.url,
          filename: file.filename,
          source: 'modrinth',
          projectId: hit.project_id,
          versionName: v.name || v.version_number,
        };
        setSelectedItems((prev) => [...prev, item]);
        if (contentType === 'mod') await resolveDependencies(hit, v);
      }
    } catch (e: any) { addToast(`Failed to add: ${e.toString()}`, 'error'); }
    setAddingId(null);
  };

  const resolveDependencies = async (mod: Hit, version: any) => {
    const deps = version.dependencies?.filter((d: any) => d.dependency_type === 'required');
    if (!deps || deps.length === 0) return;
    for (const dep of deps) {
      const depId = dep.project_id;
      if (!depId) continue;
      if (selectedItems.some((m) => m.projectId === depId)) continue;
      if (installedProjectIds?.has(depId)) continue;
      try {
        const depProject = await invoke<any>('cmd_get_modrinth_project', { id: depId });
        // Also check by slug (matches fabric mod id for mods without sidecar)
        if (depProject?.slug && installedProjectIds?.has(depProject.slug)) continue;
        const depVers = await invoke<any[]>('cmd_get_modrinth_versions', { projectId: depId, mcVersion: mcVersion ?? null, loader: versionLoader });
        if (depVers.length > 0) {
          const f = depVers[0].files?.find((f: any) => f.primary) || depVers[0].files?.[0];
          if (f) {
            setSelectedItems((prev) => [...prev, {
              name: depProject.title, iconUrl: depProject.icon_url,
              downloadUrl: f.url, filename: f.filename,
              source: 'modrinth', projectId: depId, versionName: depVers[0].name,
              isDependency: true, parentMod: mod.title,
            }]);
          }
        }
      } catch { }
    }
  };

  const removeItem = (idx: number) => {
    setSelectedItems((prev) => prev.filter((_, i) => i !== idx));
  };

  const removeByProjectId = (projectId: string) => {
    setSelectedItems((prev) => prev.filter((m) => m.projectId !== projectId));
  };

  const handleDownloadAll = async () => {
    setInstalling(true);
    let success = 0;
    let failed = 0;
    for (let i = 0; i < selectedItems.length; i++) {
      const item = selectedItems[i];
      setDownloadProgress(Math.round(((i + 1) / selectedItems.length) * 100));
      try {
        await invoke('cmd_download_to_folder', {
          instanceName,
          downloadUrl: item.downloadUrl,
          fileName: item.filename,
          subfolder,
          projectId: item.projectId || null,
          projectName: item.name || null,
          versionId: null,
          versionNumber: item.versionName || null,
          provider: item.source || 'modrinth',
        });
        success++;
      } catch (e: any) { failed++; addToast(t('content.install_error', { name: item.name, error: e.toString() }), 'error'); }
    }
    setInstalling(false);
    setDownloadProgress(0);
    addToast(t('content.install_result', { success: success.toString(), failed: failed.toString() }), success > 0 ? 'success' : 'error');
    setSelectedItems([]);
    setSubView('search');
    onInstalled();
    onClose();
  };

  const sortHits = (items: Hit[]): Hit[] => {
    const sorted = [...items];
    switch (sortMode) {
      case 'downloads': return sorted.sort((a, b) => (b.downloads || 0) - (a.downloads || 0));
      case 'newest': return sorted.sort((a, b) => (b.title || '').localeCompare(a.title || ''));
      case 'oldest': return sorted.sort((a, b) => (a.title || '').localeCompare(b.title || ''));
    }
  };

  const displayHits = results.length > 0 ? results : popular;

  const renderMarkdown = (text: string): string => {
    if (!text) return '';
    // Rewrite raw <img>/<a> HTML to markdown so the marked pipeline gets a
    // single, consistent input.
    const clean = text
      .replace(/<img\s[^>]*>/gi, (m) => { const src = m.match(/src=["']([^"']+)["']/i); return src ? `![](${src[1]})` : ''; })
      .replace(/<a[^>]+href=["']([^"']+)["'][^>]*>(.*?)<\/a>/gi, '[$2]($1)');
    try {
      const html = marked.parse(clean, { gfm: true, breaks: false }) as string;
      const sanitized = DOMPurify.sanitize(html, {
        ALLOWED_TAGS: [
          'a', 'b', 'blockquote', 'br', 'code', 'del', 'div', 'em', 'h1',
          'h2', 'h3', 'h4', 'h5', 'h6', 'hr', 'i', 'img', 'ins', 'li',
          'ol', 'p', 'pre', 's', 'span', 'strong', 'sub', 'sup', 'table',
          'tbody', 'td', 'th', 'thead', 'tr', 'ul',
        ],
        ALLOWED_ATTR: ['href', 'src', 'alt', 'title', 'class'],
        ALLOWED_URI_REGEXP: /^https?:\/\//i,
        ADD_ATTR: [],
        FORBID_TAGS: ['script', 'iframe', 'object', 'embed', 'style', 'form', 'input', 'button'],
        FORBID_ATTR: ['onerror', 'onload', 'onclick', 'onmouseover', 'onfocus', 'onblur', 'onchange', 'onsubmit', 'style'],
      });
      return sanitized;
    } catch { return ''; }
  };

  const formatDownloads = (n?: number) => {
    if (n == null) return '';
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
    return String(n);
  };

  // Confirm view
  if (subView === 'confirm') {
    const direct = selectedItems.filter((m) => !m.isDependency);
    const deps = selectedItems.filter((m) => m.isDependency);
    return (
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
        <div style={{ padding: 'var(--space-md) var(--space-xl)', borderBottom: '1px solid var(--surface-border)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)', flexShrink: 0 }}>
          <Button variant="ghost" size="sm" onClick={() => setSubView('search')}><ArrowLeft size={16} /> {t('common.back')}</Button>
          <h2 style={{ margin: 0, fontSize: 'var(--font-size-lg)', fontWeight: 700, flex: 1 }}>{t('content.confirm_heading', { label, count: selectedItems.length.toString() })}</h2>
          <Button onClick={handleDownloadAll} disabled={installing || selectedItems.length === 0} loading={installing}>
            <Download size={14} /> {t('common.install')}
          </Button>
        </div>
        {installing && (
          <div style={{ margin: 'var(--space-md) var(--space-xl)', background: 'var(--surface-glass)', borderRadius: 'var(--radius-md)', padding: 'var(--space-md)' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 4, fontSize: 'var(--font-size-sm)' }}><span>{t('common.installing')}</span><span>{downloadProgress}%</span></div>
            <div style={{ height: 4, background: 'var(--bg-tertiary)', borderRadius: 2, overflow: 'hidden' }}>
              <div style={{ height: '100%', width: `${downloadProgress}%`, background: 'var(--primary)', borderRadius: 2, transition: 'width 0.3s' }} />
            </div>
          </div>
        )}
        <div style={{ flex: 1, overflowY: 'auto', padding: 'var(--space-md) var(--space-xl)' }}>
          {direct.length > 0 && (
            <div style={{ marginBottom: 'var(--space-lg)' }}>
              <h3 style={{ fontSize: 'var(--font-size-md)', fontWeight: 600, marginBottom: 'var(--space-sm)' }}>{t('content.mods_heading', { label, count: direct.length.toString() })}</h3>
              {direct.map((item, i) => (
                <div key={i} style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', padding: '8px 12px', borderRadius: 'var(--radius-md)', background: 'var(--bg-secondary)', marginBottom: 4 }}>
                  {item.iconUrl && <img src={item.iconUrl} alt="" style={{ width: 28, height: 28, borderRadius: 4, objectFit: 'cover' }} />}
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontWeight: 500, fontSize: 'var(--font-size-sm)' }}>{item.name}</div>
                    <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{item.versionName} · {item.source}</div>
                  </div>
                  <Button size="sm" variant="ghost" onClick={() => removeItem(selectedItems.indexOf(item))}><X size={12} /></Button>
                </div>
              ))}
            </div>
          )}
          {deps.length > 0 && (
            <div style={{ marginBottom: 'var(--space-lg)' }}>
              <h3 style={{ fontSize: 'var(--font-size-md)', fontWeight: 600, marginBottom: 'var(--space-sm)', color: 'var(--warning)' }}>{t('content.deps_heading', { count: deps.length.toString() })}</h3>
              {deps.map((item, i) => (
                <div key={i} style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', padding: '8px 12px', borderRadius: 'var(--radius-md)', background: 'var(--bg-secondary)', border: '1px solid hsla(35, 90%, 55%, 0.2)', marginBottom: 4 }}>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontWeight: 500, fontSize: 'var(--font-size-sm)' }}>{item.name}</div>
                    <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{t('content.dep_required_by', { parent: item.parentMod ?? '' })}</div>
                  </div>
                  <Button size="sm" variant="ghost" onClick={() => removeItem(selectedItems.indexOf(item))}><X size={12} /></Button>
                </div>
              ))}
            </div>
          )}
          {selectedItems.length === 0 && <div style={{ textAlign: 'center', padding: 'var(--space-2xl)', color: 'var(--text-tertiary)' }}>{t('common.no_items')}</div>}
        </div>
      </div>
    );
  }

  // Search view
  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      {/* Header */}
      <div style={{ padding: 'var(--space-md) var(--space-xl)', borderBottom: '1px solid var(--surface-border)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)', flexShrink: 0 }}>
        <Button variant="ghost" size="sm" onClick={onClose}><ArrowLeft size={16} /> {t('common.back')}</Button>
        <h2 style={{ margin: 0, fontSize: 'var(--font-size-lg)', fontWeight: 700, flex: 1 }}>{t('content.browse_heading', { label })}</h2>
      </div>

      {/* Search + sort bar */}
      <div style={{ padding: 'var(--space-sm) var(--space-xl)', borderBottom: '1px solid var(--surface-border)', display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', flexShrink: 0 }}>
        <div style={{ position: 'relative', flex: 1 }}>
          <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--text-tertiary)' }} />
          <input className="input" type="text" placeholder={t('content.search_placeholder', { label: label.toLowerCase() })} value={query} onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
            style={{ paddingLeft: 32, fontSize: 'var(--font-size-sm)' }} />
        </div>
        <Button size="sm" onClick={handleSearch} disabled={loading} loading={loading}>{t('common.search')}</Button>
      </div>

      {/* Sort controls */}
      <div style={{ display: 'flex', gap: 4, alignItems: 'center', padding: 'var(--space-xs) var(--space-xl)', flexShrink: 0 }}>
        <ArrowUpDown size={12} style={{ color: 'var(--text-tertiary)' }} />
        {(['downloads', 'newest', 'oldest'] as const).map((mode) => (
          <Button key={mode} size="sm" variant={sortMode === mode ? 'primary' : 'ghost'} onClick={() => setSortMode(mode)} style={{ fontSize: '11px', padding: '4px 8px' }}>
            {mode === 'downloads' && <><Star size={10} /> {t('content.sort_downloads')}</>}
            {mode === 'newest' && <><Calendar size={10} /> {t('content.sort_newest')}</>}
            {mode === 'oldest' && <><Calendar size={10} /> {t('content.sort_oldest')}</>}
          </Button>
        ))}
      </div>

      {/* Split content */}
      <div style={{ display: 'flex', gap: 'var(--space-md)', flex: 1, overflow: 'hidden', padding: 'var(--space-sm) var(--space-xl)' }}>
        {/* Results list */}
        <div style={{ flex: selected ? '0 0 38%' : '1', overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: 4 }}>
          {loading && (
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 40, color: 'var(--text-tertiary)' }}>
              <Loader2 size={24} style={{ animation: 'spin 1s linear infinite' }} />
            </div>
          )}
          {!loading && sortHits(displayHits).length === 0 && (
            <div style={{ textAlign: 'center', padding: 40, color: 'var(--text-tertiary)' }}>
              <Package size={32} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
              <div>{t('content.empty_results', { label: label.toLowerCase() })}</div>
            </div>
          )}
          {sortHits(displayHits).map((hit) => {
            const added = isAdded(hit.project_id, hit.slug, hit.title);
            return (
              <div key={hit.project_id}
                onClick={() => handleSelect(hit)}
                style={{
                  display: 'flex', gap: 'var(--space-sm)', padding: '8px 10px', cursor: 'pointer',
                  borderRadius: 'var(--radius-md)', border: '1px solid',
                  borderColor: selected?.project_id === hit.project_id ? 'var(--primary)' : added ? 'var(--success)' : 'transparent',
                  background: selected?.project_id === hit.project_id ? 'var(--primary-dim)' : added ? 'hsla(150, 60%, 50%, 0.08)' : 'var(--bg-secondary)',
                  transition: 'all 0.15s',
                }}
              >
                {hit.icon_url ? (
                  <img src={hit.icon_url} alt="" style={{ width: 36, height: 36, borderRadius: 6, objectFit: 'cover', flexShrink: 0 }} />
                ) : (
                  <div style={{ width: 36, height: 36, borderRadius: 6, background: 'var(--surface-glass)', display: 'flex', alignItems: 'center', justifyContent: 'center', fontSize: 14, color: 'var(--text-tertiary)', flexShrink: 0 }}>
                    {hit.title.charAt(0)}
                  </div>
                )}
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', display: 'flex', alignItems: 'center', gap: 6 }}>
                    {hit.title}
                    {added && <Check size={12} color="var(--success)" />}
                  </div>
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {hit.description}
                  </div>
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', marginTop: 2 }}>
                    {hit.downloads != null && t('content.download_count', { n: formatDownloads(hit.downloads) })}
                  </div>
                </div>
                <Button size="sm" variant={isInstalled(hit.project_id, hit.slug, hit.title) ? 'ghost' : added ? 'ghost' : 'primary'} onClick={(e) => { e.stopPropagation(); isInstalled(hit.project_id, hit.slug, hit.title) ? undefined : added ? removeByProjectId(hit.project_id) : addLatest(hit); }} disabled={isInstalled(hit.project_id, hit.slug, hit.title) || addingId === hit.project_id} style={{ flexShrink: 0, alignSelf: 'center', color: isInstalled(hit.project_id, hit.slug, hit.title) ? 'var(--text-tertiary)' : added ? 'var(--color-danger)' : undefined }}>
                  {addingId === hit.project_id ? <Loader2 size={12} style={{ animation: 'spin 1s linear infinite' }} /> : isInstalled(hit.project_id, hit.slug, hit.title) ? <><Check size={12} /> {t('content.already_installed')}</> : added ? <X size={12} /> : t('content.add_btn')}
                </Button>
              </div>
            );
          })}
          <div ref={sentinelRef} style={{ height: 1 }} />
          {loadingMore && (
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 16, color: 'var(--text-tertiary)' }}>
              <Loader2 size={18} style={{ animation: 'spin 1s linear infinite' }} />
            </div>
          )}
        </div>

        {/* Detail panel */}
        {selected && (
          <div style={{ flex: 1, overflowY: 'auto', padding: 'var(--space-md)', background: 'var(--bg-secondary)', borderRadius: 'var(--radius-lg)', border: '1px solid var(--surface-border)' }}>
            {loadingDetail ? (
              <div style={{ display: 'flex', justifyContent: 'center', padding: 40 }}><Loader2 size={24} style={{ animation: 'spin 1s linear infinite' }} /></div>
            ) : (
              <>
                <div style={{ display: 'flex', gap: 'var(--space-md)', marginBottom: 'var(--space-md)' }}>
                  {selected.icon_url && <img src={selected.icon_url} alt="" style={{ width: 48, height: 48, borderRadius: 8, objectFit: 'cover' }} />}
                  <div style={{ flex: 1 }}>
                    <h3 style={{ margin: 0, fontSize: 'var(--font-size-lg)', fontWeight: 700 }}>{selected.title}</h3>
                    <p style={{ margin: 0, fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>{selected.description}</p>
                    {selected.downloads != null && (
                      <p style={{ margin: '4px 0 0', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{formatDownloads(selected.downloads)} downloads</p>
                    )}
                  </div>
                  <Button size="sm" variant={isInstalled(selected.project_id, selected.slug, selected.title) ? 'ghost' : isAdded(selected.project_id, selected.slug, selected.title) ? 'ghost' : 'primary'}
                    onClick={() => addLatest(selected)}
                    disabled={isInstalled(selected.project_id, selected.slug, selected.title) || isAdded(selected.project_id, selected.slug, selected.title) || addingId === selected.project_id}
                    style={{ flexShrink: 0, alignSelf: 'flex-start' }}>
                    {addingId === selected.project_id ? <Loader2 size={12} style={{ animation: 'spin 1s linear infinite' }} /> : isInstalled(selected.project_id, selected.slug, selected.title) ? <><Check size={12} /> {t('content.already_installed')}</> : isAdded(selected.project_id, selected.slug, selected.title) ? <><Check size={12} /> {t('content.added_btn')}</> : t('content.add_btn')}
                  </Button>
                  <X size={16} style={{ cursor: 'pointer', color: 'var(--text-tertiary)', flexShrink: 0 }} onClick={() => setSelected(null)} />
                </div>

                {/* Full description */}
                {modDetail?.body && (
                  <div className="md-content" style={{ marginBottom: 'var(--space-md)', padding: 'var(--space-md)', background: 'var(--bg-tertiary)', borderRadius: 'var(--radius-md)' }}
                    dangerouslySetInnerHTML={{ __html: renderMarkdown(modDetail.body) }}
                    onClick={(e) => {
                      const a = (e.target as HTMLElement).closest('a');
                      if (a?.href?.startsWith('http')) { e.preventDefault(); openUrl(a.href); }
                    }} />
                )}
                {modDetail?.description && !modDetail.body && (
                  <div className="md-content" style={{ marginBottom: 'var(--space-md)', padding: 'var(--space-md)', background: 'var(--bg-tertiary)', borderRadius: 'var(--radius-md)' }}>
                    {modDetail.description}
                  </div>
                )}

                {/* Version selector */}
                {versions.length > 0 && (
                  <div style={{ marginBottom: 'var(--space-md)' }}>
                    <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 'var(--space-sm)' }}>
                      <span style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600 }}>{t('content.versions_label')}</span>
                      <span style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{t('content.versions_available', { n: versions.length.toString() })}</span>
                    </div>
                    <div style={{ maxHeight: 240, overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
                      {versions.slice(0, versionLimit).map((v: any) => {
                        const file = v.files?.[0];
                        const alreadyAdded = selectedItems.some((m) => m.filename === file?.filename);
                        return (
                          <div key={v.id} onClick={() => {
                            if (!alreadyAdded && file) {
                              const item: SelectedItem = {
                                name: selected.title, iconUrl: selected.icon_url ?? undefined,
                                downloadUrl: file.url, filename: file.filename,
                                source: 'modrinth', projectId: selected.project_id,
                                versionName: v.name || v.version_number,
                              };
                              setSelectedItems((prev) => [...prev, item]);
                              addToast(t('content.added_toast', { name: selected.title }), 'success');
                            }
                          }}
                            style={{
                              display: 'flex', alignItems: 'center', justifyContent: 'space-between',
                              padding: '8px 12px', cursor: 'pointer', fontSize: 'var(--font-size-sm)',
                              background: alreadyAdded ? 'hsla(150, 60%, 50%, 0.08)' : 'transparent',
                              borderBottom: '1px solid var(--surface-border)',
                            }}>
                            <div style={{ flex: 1, overflow: 'hidden' }}>
                              <div style={{ fontWeight: 500 }}>{v.name || v.version_number}</div>
                              <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                                {v.game_versions && v.game_versions.length > 0 && <span>{v.game_versions.join(', ')} · </span>}
                                {v.date_published && new Date(v.date_published).toLocaleDateString()}
                              </div>
                            </div>
                            {alreadyAdded ? <Check size={12} color="var(--success)" /> : <span style={{ fontSize: 'var(--font-size-xs)', color: 'var(--primary)' }}>{t('content.add_btn')}</span>}
                          </div>
                        );
                      })}
                      {versionLimit < versions.length && (
                        <div ref={versionSentinelRef} style={{ height: 1 }} />
                      )}
                    </div>
                  </div>
                )}
              </>
            )}
          </div>
        )}
      </div>

      {/* Bottom bar with selected count + Confirm */}
      {selectedItems.length > 0 && (
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', padding: 'var(--space-sm) var(--space-xl)', borderRadius: 'var(--radius-md)', background: 'var(--bg-secondary)', border: '1px solid var(--surface-border)', flexShrink: 0, margin: '0 var(--space-xl) var(--space-md)' }}>
          <span style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)', flexShrink: 0 }}>{t('common.items_selected', { n: selectedItems.length.toString() })}</span>
          <div style={{ flex: 1, display: 'flex', gap: 4, overflowX: 'auto' }}>
            {selectedItems.map((m, i) => (
              <div key={i} style={{ display: 'flex', alignItems: 'center', gap: 4, padding: '2px 8px', borderRadius: 999, background: m.isDependency ? 'var(--warning-dim)' : 'var(--primary-dim)', fontSize: 11, whiteSpace: 'nowrap', flexShrink: 0 }}>
                {m.name}
                <X size={10} style={{ cursor: 'pointer' }} onClick={() => removeItem(i)} />
              </div>
            ))}
          </div>
          <Button size="sm" variant="ghost" onClick={() => setSelectedItems([])} style={{ flexShrink: 0 }}>{t('common.clear')}</Button>
          <Button size="sm" onClick={() => setSubView('confirm')} style={{ flexShrink: 0 }}>
            {t('common.confirm')}
          </Button>
        </div>
      )}
    </div>
  );
}
