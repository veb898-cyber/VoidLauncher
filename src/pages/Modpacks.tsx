import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Search, Loader2, X, Check, Download, CirclePause, CirclePlay } from 'lucide-react';
import { Button } from '../components/ui/Button';
import { EmptyState } from '../components/ui/EmptyState';
import { ResultListSkeleton } from '../components/ui/ResultListSkeleton';
import { useT } from '../lib/i18n';
import { useModpacksStore, type ModpacksTab, type MrHit, type CfHit, type AtlPack } from '../stores/modpacksStore';
import { formatBytes, formatDownloads } from '../lib/format';
import { useSettingsStore } from '../stores/settingsStore';

// ---- Remote catalog icons ---------------------------------------------------
// CDN icon URLs are never given to <img src="https://..."> directly: the
// webview follows only the system proxy with no proxy→direct fallback, so a
// single flaky host leaves every icon blank for some users. Icons are fetched
// through cmd_fetch_icon_url (send_with_fallback) and rendered as data URLs;
// session RAM cache + failure cooldown keep re-renders cheap.
const remoteIconCache = new Map<string, string>();
const remoteIconFailedAt = new Map<string, number>();
const remoteIconInflight = new Set<string>();
const remoteIconSubs = new Map<string, ((v: string | null) => void)[]>();
const REMOTE_ICON_RETRY_MS = 60_000;

function requestRemoteIcon(url: string) {
  if (remoteIconInflight.has(url)) return;
  const failedAt = remoteIconFailedAt.get(url);
  if (failedAt && Date.now() - failedAt < REMOTE_ICON_RETRY_MS) {
    setTimeout(() => {
      (remoteIconSubs.get(url) ?? []).forEach((fn) => fn(null));
      remoteIconSubs.delete(url);
    }, 0);
    return;
  }
  remoteIconInflight.add(url);
  invoke<string | null>('cmd_fetch_icon_url', { url })
    .then((data) => {
      if (data) {
        remoteIconCache.set(url, data);
        remoteIconFailedAt.delete(url);
      } else {
        remoteIconFailedAt.set(url, Date.now());
      }
      (remoteIconSubs.get(url) ?? []).forEach((fn) => fn(data ?? null));
    })
    .catch(() => {
      remoteIconFailedAt.set(url, Date.now());
      (remoteIconSubs.get(url) ?? []).forEach((fn) => fn(null));
    })
    .finally(() => {
      remoteIconInflight.delete(url);
      remoteIconSubs.delete(url);
    });
}

function RemoteIcon({ url, style, className }: { url: string; style?: React.CSSProperties; className?: string }) {
  const [src, setSrc] = useState<string | null>(() =>
    url.startsWith('data:') ? url : remoteIconCache.get(url) ?? null,
  );

  useEffect(() => {
    if (url.startsWith('data:')) {
      setSrc(url);
      return;
    }
    let alive = true;
    setSrc(remoteIconCache.get(url) ?? null);
    if (!remoteIconCache.has(url)) {
      const subs = remoteIconSubs.get(url) ?? [];
      subs.push((v) => { if (alive) setSrc(v); });
      remoteIconSubs.set(url, subs);
      requestRemoteIcon(url);
    }
    return () => { alive = false; };
  }, [url]);

  if (!src) return null;
  return (
    <img src={src} alt="" style={style} className={className} loading="lazy"
      onError={() => setSrc(null)} />
  );
}

export function Modpacks() {
  const t = useT();
  const store = useModpacksStore();
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  const curseforgeApiKey = useSettingsStore((s) => s.config?.curseforge_api_key);

  const {
    tab, query, mcFilter, loadingByTab, loadingMore,
    mrResults, cfResults, atlPacks, atlFiltered,
    selected, versions, loadingDetail, atlDetail,
    installName, nameError, installing, paused, progress,
    installVersionId, installCfFileId, installAtlVersion,
    fetchRetry, fileProgress,
  } = store;

  const loading = loadingByTab[tab];

  // Listen for retry + byte-level progress events while this page exists;
  // state itself lives in the store so it survives page switches.
  useEffect(() => {
    const unsubs: (() => void)[] = [];
    listen<any>('modpack_fetch_retry', (event) => store.onFetchRetry(event.payload)).then((fn) => unsubs.push(fn)).catch(() => {});
    listen<any>('modpack_file_progress', (event) => store.onFileProgress(event.payload)).then((fn) => unsubs.push(fn)).catch(() => {});
    listen<any>('import-progress', (event) => store.onImportProgress(event.payload)).then((fn) => unsubs.push(fn)).catch(() => {});
    return () => { unsubs.forEach((fn) => fn()); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Infinite scroll
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el) return;
    const obs = new IntersectionObserver((entries) => {
      if (entries[0].isIntersecting) store.loadMore();
    }, { rootMargin: '200px' });
    obs.observe(el);
    return () => obs.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [store.loadMore, loading]);

  // ATLauncher client-side name filter
  useEffect(() => {
    if (tab !== 'atlauncher') return;
    const q = query.trim().toLowerCase();
    if (q) {
      const filtered = atlPacks.filter((p) => p.name.toLowerCase().includes(q));
      useModpacksStore.setState({ atlFiltered: filtered });
    } else if (atlFiltered !== atlPacks) {
      useModpacksStore.setState({ atlFiltered: atlPacks });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query, tab, atlPacks]);

  // Load the initial catalog once per page mount (per tab, cached in store)
  useEffect(() => {
    const dataReady = tab === 'atlauncher'
      ? atlPacks.length > 0
      : tab === 'curseforge'
        ? cfResults.length > 0
        : mrResults.length > 0;
    if (!dataReady && !loadingByTab[tab]) {
      store.loadInitial(tab);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab]);

  const mcVersionsForAtl = (pack: AtlPack) => {
    const versions = pack.versions ?? [];
    const set = new Set(versions.map((v) => v.minecraft));
    return [...set];
  };

  const selectedMcVersions: string[] = versions.length > 0 && 'game_versions' in (versions[0] ?? {})
    ? [...new Set(versions.flatMap((v: any) => v.game_versions || []))]
    : [];

  // ATLauncher icons may arrive as data URLs, plain base64 or CDN URLs.
  const toIconSrc = (raw?: string | null): string | null => {
    if (!raw) return null;
    if (raw.startsWith('data:') || raw.startsWith('http')) return raw;
    return `data:image/png;base64,${raw}`;
  };

  const renderCard = (name: string, desc: string, icon: string | null, meta: string, onClick: () => void, keyId: string, isSelected: boolean, idx: number) => (
    <div key={keyId} className={`modpack-card stagger-in ${isSelected ? 'modpack-card--selected' : ''}`} onClick={onClick} role="button" tabIndex={0}
      style={{ animationDelay: `${Math.min(idx, 9) * 24}ms` }}
      onKeyDown={(e) => { if (e.key === 'Enter') onClick(); }}>
      <div className="modpack-card__icon modpack-card__icon--frame">
        <div className="modpack-card__icon-fallback-letter">{name.charAt(0).toUpperCase()}</div>
        {icon && <RemoteIcon url={icon} className="modpack-card__icon" />}
      </div>
      <div style={{ flex: 1, overflow: 'hidden' }}>
        <div style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>{name}</div>
        <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>{desc}</div>
        <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: 2 }}>{meta}</div>
      </div>
    </div>
  );

  const detailSelectedName = () => {
    if (!selected) return '';
    return 'title' in selected ? (selected as MrHit).title : 'name' in selected ? (selected as CfHit | AtlPack).name : '';
  };

  const detailSelectedDesc = () => {
    if (!selected) return '';
    if ('description' in selected) return (selected as MrHit | AtlPack).description || '';
    return (selected as CfHit).summary;
  };

  const detailSelectedIcon = () => {
    if (!selected) return null;
    if ('icon_url' in selected) return (selected as MrHit).icon_url;
    const cf = selected as CfHit;
    const cfIcon = cf.logo?.thumbnailUrl || cf.logo?.url;
    if (cfIcon) return cfIcon;
    const atl = selected as AtlPack;
    return toIconSrc(atl.icon);
  };

  const renderDetail = () => {
    if (!selected) return null;
    const typeOk = tab === 'atlauncher'
      ? 'safeName' in selected
      : tab === 'curseforge'
        ? 'downloadCount' in selected
        : 'project_id' in selected;
    if (!typeOk) return null;
    const icon = detailSelectedIcon();
    const mcOptions = tab === 'atlauncher'
      ? mcVersionsForAtl(selected as AtlPack)
      : selectedMcVersions;

    const installBtn = () => {
      if (tab === 'atlauncher') {
        return (
          <Button size="sm" disabled={paused ? false : (installing || !installAtlVersion)} onClick={() => paused ? store.resumeInstall() : store.installFromAtl(selected as AtlPack)}>
            {paused ? <><CirclePlay size={12} /> {t('modpacks.resume')}</> : installing ? <Loader2 size={12} style={{ animation: 'spin 1s linear infinite' }} /> : <><Download size={12} /> {t('modpacks.install_btn')}</>}
          </Button>
        );
      }
      if (tab === 'modrinth') {
        return (
          <Button size="sm" disabled={paused ? false : (installing || !installVersionId)} onClick={() => paused ? store.resumeInstall() : store.installFromMr()}>
            {paused ? <><CirclePlay size={12} /> {t('modpacks.resume')}</> : installing ? <Loader2 size={12} style={{ animation: 'spin 1s linear infinite' }} /> : <><Download size={12} /> {t('modpacks.install_btn')}</>}
          </Button>
        );
      }
      return (
        <Button size="sm" disabled={paused ? false : (installing || installCfFileId == null)} onClick={() => paused ? store.resumeInstall() : store.installFromCf(selected as CfHit)}>
          {paused ? <><CirclePlay size={12} /> {t('modpacks.resume')}</> : installing ? <Loader2 size={12} style={{ animation: 'spin 1s linear infinite' }} /> : <><Download size={12} /> {t('modpacks.install_btn')}</>}
        </Button>
      );
    };

    const retryHint = fetchRetry && loading ? (
      <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
        {t('modpacks.retry_hint', { attempt: String(fetchRetry.attempt), total: String(fetchRetry.total) })}
      </div>
    ) : null;

  const fileHint = fileProgress && fileProgress.total > 0 ? (
    <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: 4 }}>
      {t('modpacks.file_progress', {
        bytes: formatBytes(fileProgress.downloaded),
        total: formatBytes(fileProgress.total),
        percent: String(Math.round((fileProgress.downloaded / fileProgress.total) * 100)),
      })}
    </div>
  ) : null;

  // Translate backend install status messages (they are always in English).
  const progressLabel = (stage: string, msg: string): string => {
    if (stage === 'done') return t('modpacks.progress_done');
    if (stage === 'indexing') return t('modpacks.progress_indexing');
    if (stage === 'extracting') return t('modpacks.progress_extracting');
    if (stage === 'reading') return t('modpacks.progress_reading');
    if (stage === 'loader') {
      if (msg.startsWith('Installing ')) return t('modpacks.progress_installing', { text: msg.slice(11) });
      return msg;
    }
    if (stage === 'downloading-mods') {
      const nameMatch = msg.match(/^(Downloaded|Skipped|Failed):?\s+(.+?)(\s*\(|$)/);
      const name = nameMatch ? nameMatch[2] : '';
      const base = t('modpacks.progress_downloading_mods', {
        current: String(progress?.current ?? 0),
        total: String(progress?.total ?? 0),
      });
      return name ? `${base} — ${name}` : base;
    }
    return msg;
  };

    return (
      <div className="modpack-detail">
        <div style={{ display: 'flex', gap: 'var(--space-md)', marginBottom: 'var(--space-md)' }}>
          {icon && <RemoteIcon url={icon} style={{ width: 48, height: 48, borderRadius: 8, objectFit: 'cover', flexShrink: 0 }} />}
          <div style={{ flex: 1 }}>
            <h3 style={{ margin: 0, fontSize: 'var(--font-size-lg)', fontWeight: 700 }}>{detailSelectedName()}</h3>
            <p style={{ margin: 0, fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>{detailSelectedDesc()}</p>
          </div>
          <X size={16} style={{ cursor: 'pointer', color: 'var(--text-tertiary)', flexShrink: 0 }} onClick={() => store.clearSelected()} />
        </div>

        {loadingDetail && (
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>
            <Loader2 size={14} style={{ animation: 'spin 1s linear infinite' }} /> {t('modpacks.loading')}
          </div>
        )}

        {!loadingDetail && tab === 'atlauncher' && atlDetail && (
          <div>
            <div style={{ fontSize: 'var(--font-size-sm)', marginBottom: 'var(--space-sm)' }}>
              Minecraft {atlDetail.minecraft}
              {atlDetail.loader && (
                <span style={{ marginLeft: 8, fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)' }}>
                  {t('modpacks.loader', { loader: `${atlDetail.loader}${atlDetail.loaderVersion ? ' ' + atlDetail.loaderVersion : ''}` })}
                </span>
              )}
            </div>
            {atlDetail.mods.length > 0 && (
              <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginBottom: 'var(--space-md)' }}>
                {t('modpacks.mods_count', { count: atlDetail.mods.length.toString() })}
              </div>
            )}
          </div>
        )}

        {/* Version picker */}
        {!loadingDetail && versions.length > 0 && (
          <div style={{ marginBottom: 'var(--space-md)' }}>
            <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 'var(--space-sm)' }}>
              {tab === 'atlauncher' ? t('modpacks.versions_for', { mc: mcFilter || '' }) : t('common.version')}
            </div>
            <div style={{ maxHeight: 220, overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
              {versions.map((v: any) => {
                const isSel = tab === 'modrinth' ? installVersionId === v.id : tab === 'curseforge' ? installCfFileId === v.id : installAtlVersion === v.version;
                return (
                  <div key={v.id ?? v.version}
                    onClick={() => {
                      if (tab === 'modrinth') { store.setInstallVersionId(v.id); }
                      else if (tab === 'curseforge') { store.setInstallCfFileId(v.id); }
                      else { store.setInstallAtlVersion(v.version); store.selectAtl(selected as AtlPack, v.version); }
                    }}
                    style={{
                      display: 'flex', alignItems: 'center', justifyContent: 'space-between',
                      padding: '8px 12px', cursor: 'pointer', fontSize: 'var(--font-size-sm)',
                      background: isSel ? 'hsla(210, 90%, 60%, 0.1)' : 'transparent',
                      borderBottom: '1px solid var(--surface-border)',
                    }}>
                    <div style={{ flex: 1, overflow: 'hidden' }}>
                      <div style={{ fontWeight: 500 }}>
                        {v.name || v.displayName || v.version}
                        {tab === 'atlauncher' && v.isRecommended && (
                          <span style={{ marginLeft: 8, fontSize: 'var(--font-size-xs)', color: 'var(--primary)' }}>{t('modpacks.recommended')}</span>
                        )}
                      </div>
                      <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                        {tab === 'modrinth' && <span>{(v.game_versions ?? []).join(', ')} · {v.version_number} · {formatBytes(v.files?.[0]?.size ?? 0)}</span>}
                        {tab === 'curseforge' && <span>{(v.gameVersions ?? []).join(', ')} · {new Date(v.fileDate).toLocaleDateString()} · {formatBytes(v.fileLength ?? 0)}</span>}
                        {tab === 'atlauncher' && <span>Minecraft {v.minecraft}</span>}
                      </div>
                    </div>
                    {isSel && <Check size={12} color="var(--success)" />}
                  </div>
                );
              })}
            </div>
          </div>
        )}
        {!loadingDetail && versions.length === 0 && tab !== 'atlauncher' && (
          <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-tertiary)', marginBottom: 'var(--space-md)' }}>{t('modpacks.no_versions')}</div>
        )}

        {/* MC version filter for CurseForge/ATLauncher */}
        {!loadingDetail && tab !== 'modrinth' && mcOptions.length > 1 && (
          <div style={{ marginBottom: 'var(--space-md)' }}>
            <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginBottom: 4 }}>Minecraft</div>
            <select className="input" style={{ width: '100%' }} value={mcFilter}
              onChange={(e) => {
                store.setMcFilter(e.target.value);
                store.setInstallCfFileId(null);
                store.setInstallAtlVersion('');
                if (tab === 'curseforge') {
                  import('@tauri-apps/api/core').then(({ invoke }) => {
                    invoke<any>('cmd_get_curseforge_modpack_files', { modId: (selected as CfHit).id, mcVersion: e.target.value || null, loader: null })
                      .then((res) => useModpacksStore.setState({ versions: res.data || [] }))
                      .catch(() => {});
                  });
                } else if (tab === 'atlauncher') {
                  const pack = selected as AtlPack;
                  const versions = e.target.value ? pack.versions.filter((v) => v.minecraft === e.target.value) : pack.versions;
                  useModpacksStore.setState({ versions });
                }
              }}>
              <option value="">{t('common.all')}</option>
              {mcOptions.map((m) => <option key={m} value={m}>{m}</option>)}
            </select>
          </div>
        )}

        {/* Install */}
        <div style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'center' }}>
          <input
            className={`input ${nameError ? 'input--error' : ''}`}
            placeholder={t('modpacks.name_placeholder')}
            value={installName}
            onChange={(e) => { store.setInstallName(e.target.value); if (nameError) store.setNameError(false); }}
            style={{ flex: 1, minWidth: 0 }}
            disabled={installing}
          />
          {installing && !paused && (
            <Button size="sm" variant="ghost" onClick={() => store.pauseInstall()}>
              <CirclePause size={12} /> {t('modpacks.pause')}
            </Button>
          )}
          {installBtn()}
        </div>

        {(progress || installing || paused) && (
          <div style={{ marginTop: 'var(--space-md)' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', marginBottom: 4 }}>
              <span>{paused ? t('modpacks.paused') : (progress ? progressLabel(progress.stage, progress.message) : t('modpacks.installing'))}</span>
              <span>{progress && progress.total > 0 ? `${Math.round((progress.current / progress.total) * 100)}%` : ''}</span>
            </div>
            <div style={{ height: 6, background: 'var(--bg-tertiary)', borderRadius: 999, overflow: 'hidden' }}>
              <div className="progress-fill" style={{
                height: '100%',
                width: progress && progress.total > 0 ? `${Math.min(100, (progress.current / progress.total) * 100)}%` : '100%',
                background: paused ? 'var(--text-tertiary)' : 'var(--primary)',
                transition: 'width 0.2s ease',
                animation: !paused && (!progress || progress.total === 0) ? 'indeterminate 1.2s ease-in-out infinite' : undefined,
              }} />
            </div>
            {!paused && fileHint}
          </div>
        )}

        {retryHint}
      </div>
    );
  };

  return (
    <div className="page animate-fade-in">
      <div className="page__header">
        <div>
          <h1 className="page__title">{t('modpacks.title')}</h1>
          <p className="page__subtitle">{t('modpacks.subtitle')}</p>
        </div>
      </div>

      {/* Tabs */}
      <div className="tabs" style={{ marginBottom: 'var(--space-md)' }}>
        {(['modrinth', 'curseforge', 'atlauncher'] as ModpacksTab[]).map((tb) => (
          <button key={tb} className={`tab ${tab === tb ? 'tab--active' : ''}`} onClick={() => store.switchTab(tb)}>
            {t(`modpacks.tab_${tb}`)}
          </button>
        ))}
      </div>

      {/* Search */}
      <div className="search-bar" style={{ marginBottom: 'var(--space-md)' }}>
        <Search size={16} style={{ color: 'var(--text-tertiary)', flexShrink: 0 }} />
        <input
          className="input input--bare"
          placeholder={t('modpacks.search_placeholder')}
          value={query}
          onChange={(e) => store.setQuery(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') store.search(); }}
          style={{ paddingLeft: 4 }}
        />
        {query && <X size={14} style={{ cursor: 'pointer', color: 'var(--text-tertiary)' }} onClick={() => { store.setQuery(''); store.loadInitial(tab); }} />}
      </div>

      {tab === 'curseforge' && !curseforgeApiKey && (
        <div style={{ padding: 'var(--space-lg)', background: 'var(--warning-dim)', borderRadius: 'var(--radius-md)', color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
          {t('modpacks.curseforge_key_missing')}
        </div>
      )}

      <div className="modpacks-main" style={{ display: 'flex', gap: 'var(--space-lg)', height: 'calc(100vh - 260px)', minHeight: 320 }}>
        {/* Results list */}
        <div style={{ flex: 1, overflowY: 'auto', paddingRight: 2 }}>
          {loading && (
            <div>
              <ResultListSkeleton variant="card" rows={6} />
              {fetchRetry && (
                <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', padding: 'var(--space-sm) var(--space-md)' }}>
                  {t('modpacks.retry_hint', { attempt: String(fetchRetry.attempt), total: String(fetchRetry.total) })}
                </div>
              )}
            </div>
          )}
          {!loading && tab === 'modrinth' && mrResults.length === 0 && (
            <EmptyState icon={<Search size={28} />} title={t('modpacks.empty')} compact />
          )}
          {!loading && tab === 'curseforge' && cfResults.length === 0 && (
            <EmptyState icon={<Search size={28} />} title={t('modpacks.empty')} compact />
          )}
          {!loading && tab === 'atlauncher' && atlFiltered.length === 0 && (
            <EmptyState icon={<Search size={28} />} title={t('modpacks.empty')} compact />
          )}

          {tab === 'modrinth' && mrResults.map((h, i) => renderCard(
            h.title, h.description,
            h.icon_url,
            t('modpacks.downloads_count', { count: formatDownloads(h.downloads) }),
            () => store.selectMr(h),
            h.project_id,
            !!selected && 'project_id' in selected && (selected as MrHit).project_id === h.project_id,
            i,
          ))}

          {tab === 'curseforge' && cfResults.map((h, i) => renderCard(
            h.name, h.summary,
            h.logo?.thumbnailUrl || h.logo?.url || null,
            t('modpacks.downloads_count', { count: formatDownloads(h.downloadCount) }),
            () => store.selectCf(h),
            String(h.id),
            !!selected && 'downloadCount' in selected && (selected as CfHit).id === h.id,
            i,
          ))}

          {tab === 'atlauncher' && atlFiltered.map((p, i) => renderCard(
            p.name,
            p.description ?? '',
            toIconSrc(p.icon),
            (p.versions[0] ? `Minecraft ${p.versions[0].minecraft}` : ''),
            () => store.selectAtlPack(p),
            p.safeName,
            !!selected && 'safeName' in selected && (selected as AtlPack).safeName === p.safeName,
            i,
          ))}

          {loadingMore && (
            <div style={{ display: 'flex', justifyContent: 'center', padding: 'var(--space-md)' }}>
              <Loader2 size={14} style={{ animation: 'spin 1s linear infinite', color: 'var(--text-tertiary)' }} />
            </div>
          )}
          <div ref={sentinelRef} style={{ height: 1 }} />
        </div>

        {/* Detail panel */}
        {selected && (
          <div className="modpack-detail-panel" style={{ width: 340, flexShrink: 0, overflowY: 'auto' }}>
            {renderDetail()}
          </div>
        )}
      </div>
    </div>
  );
}