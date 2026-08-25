import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Search, ArrowLeft, Package, ArrowUpDown, Star, Calendar, Loader2, X, Check, Download } from 'lucide-react';
import { Button } from '../ui/Button';
import { addToast } from '../ui/Toast';
import { t } from '../../lib/i18n';
import { openUrl } from '@tauri-apps/plugin-opener';
import { renderMarkdownToHtml, hydrateRemoteImages } from '../../lib/markdown';

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

interface Props {
  instanceName: string;
  packType: 'resourcepacks' | 'shaderpacks';
  onClose: () => void;
  onInstalled: () => void;
}

type SortMode = 'downloads' | 'newest' | 'oldest';

export function PackBrowser({ instanceName, packType, onClose, onInstalled }: Props) {
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<Hit[]>([]);
  const [loading, setLoading] = useState(false);
  const [popular, setPopular] = useState<Hit[]>([]);
  const [selected, setSelected] = useState<Hit | null>(null);
  const [modDetail, setModDetail] = useState<any>(null);
  const [loadingDetail, setLoadingDetail] = useState(false);
  const [versions, setVersions] = useState<any[]>([]);
  const [sortMode, setSortMode] = useState<SortMode>('downloads');
  const [installing, setInstalling] = useState(false);
  const [installedVersion, setInstalledVersion] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const mdBodyRef = useRef<HTMLDivElement | null>(null);
  // Re-route remote description images through the backend after render
  // (the webview alone has no proxy fallback).
  useEffect(() => {
    if (!loadingDetail) hydrateRemoteImages(mdBodyRef.current);
  }, [loadingDetail, modDetail]);

  const label = packType === 'resourcepacks' ? t('content.type_resourcepack') : t('content.type_shader');
  const projectType = packType === 'resourcepacks' ? 'resourcepack' : 'shader';

  const loadPopular = useCallback(async () => {
    setLoading(true);
    try {
      const res = await invoke<any>('cmd_popular_modrinth', { projectType, mcVersion: null, loader: null, offset: 0, limit: 50 });
      setPopular(res.hits || []);
      setLoaded(true);
    } catch { }
    setLoading(false);
  }, [projectType]);

  useEffect(() => { loadPopular(); }, [loadPopular]);

  const handleSearch = async () => {
    if (!query.trim()) {
      // Clearing the search must return to the default "popular" list;
      // otherwise stale results would keep being displayed.
      setResults([]);
      setLoaded(true);
      setSelected(null);
      loadPopular();
      return;
    }
    setLoading(true);
    setSelected(null);
    try {
      const res = await invoke<any>('cmd_search_modrinth', { query, projectType, mcVersion: null, loader: null, offset: 0, limit: 30 });
      setResults(res.hits || []);
    } catch (e: any) { addToast(t('pack.search_error', { error: e.toString() }), 'error'); }
    setLoading(false);
  };

  const handleSelect = async (hit: Hit) => {
    setSelected(hit);
    setLoadingDetail(true);
    setModDetail(null);
    setVersions([]);
    setInstalledVersion(null);
    try {
      const [detail, vers] = await Promise.all([
        invoke<any>('cmd_get_modrinth_project', { id: hit.project_id }),
        invoke<any[]>('cmd_get_modrinth_versions', { projectId: hit.project_id, mcVersion: null, loader: null }),
      ]);
      setModDetail(detail);
      setVersions(vers);
    } catch (e: any) { addToast(t('pack.detail_error', { error: e.toString() }), 'error'); }
    setLoadingDetail(false);
  };

  const handleInstall = async (version: any) => {
    setInstalling(true);
    try {
      const file = version.files?.[0];
      if (!file) { addToast(t('pack.no_file'), 'error'); setInstalling(false); return; }
      await invoke('cmd_download_to_folder', {
        instanceName,
        downloadUrl: file.url,
        fileName: file.filename,
        subfolder: packType,
        projectId: selected?.project_id || null,
        projectName: selected?.title || null,
        versionId: version.id || null,
        versionNumber: version.version_number || version.name || null,
        provider: 'modrinth',
        expectedSha1: file.hashes?.sha1 || '',
      });
      addToast(t('pack.installed_toast', { title: selected?.title || '', version: version.name || version.version_number }), 'success');
      setInstalledVersion(version.version_number || version.name);
      onInstalled();
    } catch (e: any) { addToast(t('pack.install_error', { error: e.toString() }), 'error'); }
    setInstalling(false);
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

  const formatDownloads = (n?: number) => {
    if (n == null) return '';
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
    return String(n);
  };

  // Full CommonMark+GFM rendering via marked (shared module).
  const renderMarkdown = (text: string): string => renderMarkdownToHtml(text);

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      {/* Header */}
      <div style={{ padding: 'var(--space-md) var(--space-xl)', borderBottom: '1px solid var(--surface-border)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)', flexShrink: 0 }}>
        <Button variant="ghost" size="sm" onClick={onClose}><ArrowLeft size={16} /> {t('common.back')}</Button>
        <h2 style={{ margin: 0, fontSize: 'var(--font-size-lg)', fontWeight: 700, flex: 1 }}>
          {t('pack.browse_heading', { label })}
        </h2>
      </div>

      {/* Search + sort bar */}
      <div style={{ padding: 'var(--space-sm) var(--space-xl)', borderBottom: '1px solid var(--surface-border)', display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', flexShrink: 0 }}>
        <div style={{ position: 'relative', flex: 1 }}>
          <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--text-tertiary)' }} />
          <input className="input" type="text" value={query} onChange={(e) => setQuery(e.target.value)}
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
          {!loading && sortHits(displayHits).length === 0 && loaded && (
            <div style={{ textAlign: 'center', padding: 40, color: 'var(--text-tertiary)' }}>
              <Package size={32} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
              <div>{t('pack.empty_results')}</div>
            </div>
          )}
          {sortHits(displayHits).map((hit) => (
            <div key={hit.project_id}
              onClick={() => handleSelect(hit)}
              style={{
                display: 'flex', gap: 'var(--space-sm)', padding: '8px 10px', cursor: 'pointer',
                borderRadius: 'var(--radius-md)', border: '1px solid',
                borderColor: selected?.project_id === hit.project_id ? 'var(--primary)' : 'transparent',
                background: selected?.project_id === hit.project_id ? 'var(--primary-dim)' : 'var(--bg-secondary)',
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
                <div style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {hit.title}
                </div>
                <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {hit.description}
                </div>
                <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', marginTop: 2 }}>
                  {hit.downloads != null && t('pack.download_count', { n: formatDownloads(hit.downloads) })}
                </div>
              </div>
            </div>
          ))}
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
                      <p style={{ margin: '4px 0 0', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{t('pack.download_count', { n: formatDownloads(selected.downloads) })}</p>
                    )}
                  </div>
                  <X size={16} style={{ cursor: 'pointer', color: 'var(--text-tertiary)', flexShrink: 0 }} onClick={() => setSelected(null)} />
                </div>

                {/* Full description */}
                {modDetail?.body && (
                  <div ref={mdBodyRef} className="md-content" style={{ marginBottom: 'var(--space-md)', padding: 'var(--space-md)', background: 'var(--bg-tertiary)', borderRadius: 'var(--radius-md)' }}
                    dangerouslySetInnerHTML={{ __html: renderMarkdown(modDetail.body) }}
                    onClick={(e) => { const a = (e.target as HTMLElement).closest('a'); if (a?.href?.startsWith('http')) { e.preventDefault(); openUrl(a.href); } }} />
                )}
                {modDetail?.description && !modDetail.body && (
                  <div ref={modDetail.description ? mdBodyRef : undefined} className="md-content" style={{ marginBottom: 'var(--space-md)', padding: 'var(--space-md)', background: 'var(--bg-tertiary)', borderRadius: 'var(--radius-md)' }}
                    dangerouslySetInnerHTML={{ __html: renderMarkdown(modDetail.description) }}
                    onClick={(e) => { const a = (e.target as HTMLElement).closest('a'); if (a?.href?.startsWith('http')) { e.preventDefault(); openUrl(a.href); } }} />
                )}

                {/* Version selector */}
                {versions.length > 0 && (
                  <div style={{ marginBottom: 'var(--space-md)' }}>
                    <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 'var(--space-sm)' }}>
                      <span style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600 }}>{t('pack.versions_label')}</span>
                      <span style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{t('pack.versions_available', { n: versions.length.toString() })}</span>
                    </div>
                    <div style={{ maxHeight: 240, overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
                      {versions.slice(0, 20).map((v: any) => {
                        const isInstalled = installedVersion === (v.version_number || v.name);
                        return (
                          <div key={v.id} style={{
                            display: 'flex', alignItems: 'center', justifyContent: 'space-between',
                            padding: '8px 12px', fontSize: 'var(--font-size-sm)',
                            background: isInstalled ? 'hsla(150, 60%, 50%, 0.08)' : 'transparent',
                            borderBottom: '1px solid var(--surface-border)',
                          }}>
                            <div style={{ flex: 1, overflow: 'hidden' }}>
                              <div style={{ fontWeight: 500 }}>{v.name || v.version_number}</div>
                              <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                                {v.game_versions && v.game_versions.length > 0 && <span>{v.game_versions.join(', ')} В· </span>}
                                {v.date_published && new Date(v.date_published).toLocaleDateString()}
                              </div>
                            </div>
                            {isInstalled ? (
                              <span style={{ display: 'flex', alignItems: 'center', gap: 4, fontSize: 'var(--font-size-xs)', color: 'var(--success)' }}>
                                <Check size={12} /> {t('pack.status_installed')}
                              </span>
                            ) : (
                              <Button size="sm" onClick={() => handleInstall(v)} disabled={installing} loading={installing}>
                                <Download size={12} /> {t('common.install')}
                              </Button>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  </div>
                )}
              </>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
