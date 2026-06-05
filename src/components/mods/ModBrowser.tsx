import { useEffect, useState } from 'react';
import { t } from '../../lib/i18n';
import { invoke } from '@tauri-apps/api/core';
import { Search, X, Check, Loader2, ArrowUpDown, Star, Calendar } from 'lucide-react';
import { Button } from '../ui/Button';
import { addToast } from '../ui/Toast';
import { openUrl } from '@tauri-apps/plugin-opener';

interface ModBrowserProps {
  mcVersion: string;
  loader: string;
  onConfirm: (mods: SelectedMod[]) => void;
  onCancel: () => void;
}

interface ModResult {
  id: string | number;
  name: string;
  description: string;
  downloads?: number;
  icon_url?: string;
  logo?: { thumbnail_url: string };
  author?: string;
  authors?: { name: string }[];
  source: 'modrinth' | 'curseforge';
  date_modified?: string;
}

export interface SelectedMod {
  name: string;
  iconUrl?: string;
  downloadUrl: string;
  filename: string;
  source: 'modrinth' | 'curseforge';
  modId: string | number;
  versionName?: string;
  isDependency?: boolean;
  parentMod?: string;
}

type SortMode = 'downloads' | 'newest' | 'oldest';

export function ModBrowser({ mcVersion, loader, onConfirm, onCancel }: ModBrowserProps) {
  const [source, setSource] = useState<'modrinth' | 'curseforge'>('modrinth');
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<ModResult[]>([]);
  const [searching, setSearching] = useState(false);
  const [selectedMod, setSelectedMod] = useState<ModResult | null>(null);
  const [modDetail, setModDetail] = useState<any>(null);
  const [loadingDetail, setLoadingDetail] = useState(false);
  const [versions, setVersions] = useState<any[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [sortMode, setSortMode] = useState<SortMode>('downloads');
  const [selectedMods, setSelectedMods] = useState<SelectedMod[]>([]);
  const [addingModId, setAddingModId] = useState<string | number | null>(null);

  useEffect(() => { loadPopular(); }, [source, mcVersion, loader]);

  const loadPopular = async () => {
    setSearching(true);
    setSelectedMod(null);
    try {
      if (source === 'modrinth') {
        const resp = await invoke<any>('cmd_popular_modrinth', { projectType: 'mod', mcVersion, loader, offset: 0, limit: 30 });
        setResults(resp.hits.map((h: any) => ({
          id: h.project_id, name: h.title, description: h.description,
          downloads: h.downloads, icon_url: h.icon_url, author: h.author, source: 'modrinth',
        })));
      } else {
        const resp = await invoke<any>('cmd_popular_curseforge', { mcVersion, loader, limit: 30 });
        setResults(resp.data.map((m: any) => ({
          id: m.id, name: m.name, description: m.summary,
          downloads: m.download_count, logo: m.logo,
          author: m.authors?.[0]?.name, source: 'curseforge', date_modified: m.date_modified,
        })));
      }
      setLoaded(true);
    } catch (e: any) { addToast(t('mod.load_error', { error: e.toString() }), 'error'); }
    setSearching(false);
  };

  const search = async () => {
    if (!query.trim()) { loadPopular(); return; }
    setSearching(true);
    setSelectedMod(null);
    try {
      if (source === 'modrinth') {
        const resp = await invoke<any>('cmd_search_modrinth', { query, projectType: 'mod', mcVersion, loader, offset: 0, limit: 30 });
        setResults(resp.hits.map((h: any) => ({
          id: h.project_id, name: h.title, description: h.description,
          downloads: h.downloads, icon_url: h.icon_url, author: h.author, source: 'modrinth',
        })));
      } else {
        const resp = await invoke<any>('cmd_search_curseforge', { query, mcVersion, loader, offset: 0, limit: 30 });
        setResults(resp.data.map((m: any) => ({
          id: m.id, name: m.name, description: m.summary,
          downloads: m.download_count, logo: m.logo, author: m.authors?.[0]?.name,
          source: 'curseforge', date_modified: m.date_modified,
        })));
      }
    } catch (e: any) { addToast(t('mod.search_error', { error: e.toString() }), 'error'); }
    setSearching(false);
  };

  const sortResults = (items: ModResult[]): ModResult[] => {
    const sorted = [...items];
    switch (sortMode) {
      case 'downloads': return sorted.sort((a, b) => (b.downloads || 0) - (a.downloads || 0));
      case 'newest': return sorted.sort((a, b) => (b.date_modified || '').localeCompare(a.date_modified || ''));
      case 'oldest': return sorted.sort((a, b) => (a.date_modified || '').localeCompare(b.date_modified || ''));
    }
  };

  const selectMod = async (mod: ModResult) => {
    setSelectedMod(mod);
    setLoadingDetail(true);
    setModDetail(null);
    setVersions([]);
    try {
      if (mod.source === 'modrinth') {
        const [detail, vers] = await Promise.all([
          invoke<any>('cmd_get_modrinth_project', { id: String(mod.id) }),
          invoke<any[]>('cmd_get_modrinth_versions', { projectId: String(mod.id), mcVersion, loader }),
        ]);
        setModDetail(detail);
        setVersions(vers);
      } else {
        const [detail, vers] = await Promise.all([
          invoke<any>('cmd_get_curseforge_mod_detail', { modId: Number(mod.id) }),
          invoke<any[]>('cmd_get_curseforge_files', { modId: Number(mod.id), mcVersion, loader }),
        ]);
        setModDetail(detail);
        setVersions(vers.map((v: any) => ({
          id: String(v.id), name: v.display_name || v.file_name, version_number: v.file_name,
          game_versions: v.game_versions, files: [{ url: v.download_url, filename: v.file_name, primary: true, size: v.file_length }],
          dependencies: v.dependencies, date_published: v.file_date,
        })));
      }
    } catch (e: any) { addToast(t('mod.detail_error', { error: e.toString() }), 'error'); }
    setLoadingDetail(false);
  };

  const addModLatest = async (mod: ModResult) => {
    if (selectedMods.some((m) => String(m.modId) === String(mod.id))) {
      addToast(t('mod.already_added'), 'info');
      return;
    }
    setAddingModId(mod.id);
    try {
      let vers: any[] = [];
      if (mod.source === 'modrinth') {
        vers = await invoke<any[]>('cmd_get_modrinth_versions', { projectId: String(mod.id), mcVersion, loader });
      } else {
        vers = await invoke<any[]>('cmd_get_curseforge_files', { modId: Number(mod.id), mcVersion, loader });
      }
      if (vers.length === 0) {
        addToast(t('mod.no_compatible_version'), 'warning');
        setAddingModId(null);
        return;
      }
      const v = vers[0];
      let file: any;
      if (mod.source === 'modrinth') {
        file = v.files?.find((f: any) => f.primary) || v.files?.[0];
        if (file) {
          const newMod: SelectedMod = {
            name: mod.name, iconUrl: mod.icon_url || mod.logo?.thumbnail_url,
            downloadUrl: file.url, filename: file.filename,
            source: 'modrinth', modId: mod.id, versionName: v.name || v.version_number,
          };
          setSelectedMods((prev) => [...prev, newMod]);
          addToast(t('mod.added_toast', { name: mod.name }), 'success');
          await resolveDependencies(mod, v, 'modrinth');
        }
      } else {
        file = { url: v.download_url, filename: v.file_name };
        if (file.url) {
          const newMod: SelectedMod = {
            name: mod.name, iconUrl: mod.logo?.thumbnail_url,
            downloadUrl: file.url, filename: file.filename,
            source: 'curseforge', modId: mod.id, versionName: v.display_name || v.file_name,
          };
          setSelectedMods((prev) => [...prev, newMod]);
          addToast(t('mod.added_toast', { name: mod.name }), 'success');
        }
      }
    } catch (e: any) { addToast(t('mod.add_error', { error: e.toString() }), 'error'); }
    setAddingModId(null);
  };

  const resolveDependencies = async (mod: ModResult, version: any, source: string) => {
    const deps = version.dependencies?.filter((d: any) =>
      (d.dependency_type === 'required' || d.relation_type === 3)
    );
    if (!deps || deps.length === 0) return;
    for (const dep of deps) {
      const depId = dep.project_id || dep.mod_id;
      if (!depId) continue;
      if (selectedMods.some((m) => String(m.modId) === String(depId))) continue;
      try {
        if (source === 'modrinth') {
          const depProject = await invoke<any>('cmd_get_modrinth_project', { id: depId });
          const depVers = await invoke<any[]>('cmd_get_modrinth_versions', { projectId: depId, mcVersion, loader });
          if (depVers.length > 0) {
            const f = depVers[0].files?.find((f: any) => f.primary) || depVers[0].files?.[0];
            if (f) {
              setSelectedMods((prev) => [...prev, {
                name: depProject.title, iconUrl: depProject.icon_url,
                downloadUrl: f.url, filename: f.filename,
                source: 'modrinth', modId: depId, versionName: depVers[0].name,
                isDependency: true, parentMod: mod.name,
              }]);
              addToast(t('mod.added_dep_toast', { name: depProject.title }), 'info');
            }
          }
        }
      } catch { }
    }
  };

  const removeMod = (idx: number) => {
    setSelectedMods((prev) => prev.filter((_, i) => i !== idx));
  };

  const getIcon = (mod: ModResult) => mod.icon_url || mod.logo?.thumbnail_url;
  const formatDownloads = (n?: number) => {
    if (n == null) return '';
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
    return String(n);
  };

  const resolveUrl = (url: string, src?: string): string => {
    if (!url) return url;
    const trimmed = url.trim();
    if (trimmed.startsWith('//')) return `https:${trimmed}`;
    if (trimmed.startsWith('/')) {
      const base = src === 'curseforge' ? 'https://www.curseforge.com' : 'https://modrinth.com';
      return `${base}${trimmed}`;
    }
    return trimmed;
  };

  const renderMarkdown = (text: string, source?: string): any[] => {
    if (!text) return [];
    let processed = text
      .replace(/<img\s[^>]*>/gi, (m) => {
        const src = m.match(/src=["']([^"']+)["']/i);
        const altMatch = m.match(/alt=["']([^"']*)["']/i);
        const alt = altMatch ? altMatch[1] : '';
        return src ? `![${alt}](${resolveUrl(src[1], source)})` : m;
      })
      .replace(/<a[^>]+href=["']([^"']+)["'][^>]*>(.*?)<\/a>/gi, '[$2]($1)')
      .replace(/<br\s*\/?>/gi, '\n')
      .replace(/<\/?p[^>]*>/gi, '\n')
      .replace(/<\/?strong[^>]*>/gi, '**')
      .replace(/<\/?b[^>]*>/gi, '**')
      .replace(/<\/?em[^>]*>/gi, '*')
      .replace(/<\/?i[^>]*>/gi, '*')
      .replace(/<\/?h[1-6][^>]*>/gi, '\n')
      .replace(/<\/?ul[^>]*>/gi, '')
      .replace(/<\/?ol[^>]*>/gi, '')
      .replace(/<li[^>]*>/gi, '- ')
      .replace(/<\/li>/gi, '\n')
      .replace(/<hr\s*\/?>/gi, '---')
      .replace(/<code[^>]*>(.*?)<\/code>/gi, '`$1`')
      .replace(/<pre[^>]*>(.*?)<\/pre>/gi, '\n```\n$1\n```\n')
      .replace(/<[^>]+>/g, '');

    const lines = processed.split('\n').map(l => l.replace(/\r$/, ''));
    const elements: any[] = [];
    let i = 0;
    let keyCounter = 0;

    while (i < lines.length) {
      const line = lines[i];
      if (line.trim() === '') { i++; continue; }

      if (line.trim().startsWith('```')) {
        const codeLines: string[] = [];
        const lang = line.trim().slice(3).trim();
        i++;
        while (i < lines.length && !lines[i].trim().startsWith('```')) {
          codeLines.push(lines[i]); i++;
        }
        if (i < lines.length) i++;
        elements.push(
          <div key={`code${keyCounter++}`} style={{ margin: '8px 0', borderRadius: 'var(--radius-md)', overflow: 'hidden', background: 'hsla(0,0%,0%,0.3)', border: '1px solid var(--surface-border)' }}>
            {lang && <div style={{ padding: '4px 12px', fontSize: 10, color: 'var(--text-tertiary)', borderBottom: '1px solid var(--surface-border)', fontFamily: 'monospace' }}>{lang}</div>}
            <pre style={{ margin: 0, padding: '12px', fontSize: 'var(--font-size-xs)', fontFamily: "'Cascadia Code','Fira Code',monospace", color: 'var(--text-secondary)', overflowX: 'auto', whiteSpace: 'pre', lineHeight: 1.5 }}>{codeLines.join('\n')}</pre>
          </div>
        );
        continue;
      }

      if (/^(-{3,}|\*{3,}|_{3,})\s*$/.test(line.trim())) {
        elements.push(<hr key={`hr${keyCounter++}`} style={{ border: 'none', borderTop: '1px solid var(--surface-border)', margin: '12px 0' }} />);
        i++; continue;
      }

      const headingMatch = line.match(/^(#{1,6})\s+(.+)/);
      if (headingMatch) {
        const level = headingMatch[1].length;
        const sz = level === 1 ? '1.2em' : level === 2 ? '1.1em' : '1em';
        const Tag = `h${level}` as any;
        elements.push(<Tag key={`h${keyCounter++}`} style={{ fontWeight: 700, margin: '12px 0 4px', fontSize: sz, color: 'var(--text-primary)' }}>{renderInline(headingMatch[2], source)}</Tag>);
        i++; continue;
      }

      if (line.startsWith('>')) {
        const quoteLines: string[] = [];
        while (i < lines.length && lines[i].startsWith('>')) { quoteLines.push(lines[i].replace(/^>\s?/, '')); i++; }
        elements.push(
          <blockquote key={`bq${keyCounter++}`} style={{ margin: '8px 0', padding: '8px 16px', borderLeft: '3px solid var(--primary)', background: 'hsla(265,100%,65%,0.05)', borderRadius: '0 var(--radius-sm) var(--radius-sm) 0', fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
            {quoteLines.map((ql, qi) => <div key={qi}>{renderInline(ql, source)}</div>)}
          </blockquote>
        );
        continue;
      }

      if (line.includes('|') && line.trim().startsWith('|')) {
        const tableRows: string[][] = [];
        while (i < lines.length && lines[i].includes('|') && lines[i].trim().startsWith('|')) {
          const cells = lines[i].split('|').filter((_c, idx, arr) => idx > 0 && idx < arr.length - 1).map((c) => c.trim());
          if (!cells.every((c) => /^[-:]+$/.test(c))) tableRows.push(cells);
          i++;
        }
        if (tableRows.length > 0) {
          elements.push(
            <table key={`tbl${keyCounter++}`} style={{ width: '100%', borderCollapse: 'collapse', margin: '8px 0', fontSize: 'var(--font-size-sm)' }}>
              <thead><tr>{tableRows[0].map((cell, ci) => <th key={ci} style={{ padding: '6px 10px', borderBottom: '2px solid var(--surface-border)', textAlign: 'left', fontWeight: 600, color: 'var(--text-primary)' }}>{renderInline(cell, source)}</th>)}</tr></thead>
              <tbody>{tableRows.slice(1).map((row, ri) => <tr key={ri}>{row.map((cell, ci) => <td key={ci} style={{ padding: '6px 10px', borderBottom: '1px solid var(--surface-border)', color: 'var(--text-secondary)' }}>{renderInline(cell, source)}</td>)}</tr>)}</tbody>
            </table>
          );
        }
        continue;
      }

      const imgMatch = line.match(/^!\[([^\]]*)\]\(([^)]*(?:\([^)]*\)[^)]*)*)\)\s*$/);
      if (imgMatch) {
        const url = resolveUrl(imgMatch[2], source);
        const alt = imgMatch[1];
        elements.push(
          <div key={`img${keyCounter++}`} style={{ margin: '8px 0' }}>
            <a href={url} style={{ color: 'var(--primary)', textDecoration: 'none', fontSize: 'var(--font-size-sm)' }}>
              <img src={url} alt={alt} style={{ maxWidth: '100%', borderRadius: 'var(--radius-md)', cursor: 'pointer', display: 'block' }}
                onError={(e) => {
                  const img = e.target as HTMLImageElement;
                  const parent = img.parentElement;
                  if (parent) parent.textContent = alt || url;
                }} />
            </a>
          </div>
        );
        i++; continue;
      }

      const looseImgMatch = line.match(/^!\[([^\]]*)\]\(([^)\s]+)\s*$/);
      if (looseImgMatch) {
        const url = resolveUrl(looseImgMatch[2], source);
        const alt = looseImgMatch[1];
        elements.push(<div key={`link${keyCounter++}`} style={{ margin: '8px 0' }}><a href={url} style={{ color: 'var(--primary)', textDecoration: 'none', fontSize: 'var(--font-size-sm)' }}>{alt || url}</a></div>);
        i++; continue;
      }

      const listMatch = line.match(/^[-*+]\s+(.+)/);
      if (listMatch) {
        const items: string[] = [listMatch[1]];
        while (i + 1 < lines.length) {
          const nextMatch = lines[i + 1].match(/^[-*+]\s+(.+)/);
          if (nextMatch) { items.push(nextMatch[1]); i++; } else break;
        }
        elements.push(<ul key={`ul${keyCounter++}`} style={{ margin: '4px 0', paddingLeft: '20px', fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>{items.map((item, j) => <li key={j} style={{ marginBottom: 2 }}>{renderInline(item, source)}</li>)}</ul>);
        i++; continue;
      }

      elements.push(<p key={`p${keyCounter++}`} style={{ margin: '4px 0', color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)', lineHeight: 1.6 }}>{renderInline(line, source)}</p>);
      i++;
    }
    return elements;
  };

  const renderInline = (text: string, source?: string): any[] => {
    if (!text) return [];
    const parts: any[] = [];
    let remaining = text;
    let keyIdx = 0;

    while (remaining.length > 0) {
      const codeMatch = remaining.match(/`([^`]+)`/);
      const imgMatch = remaining.match(/!\[([^\]]*)\]\(([^)]*(?:\([^)]*\)[^)]*)*)\)/);
      const linkMatch = remaining.match(/\[([^\]]+)\]\(([^)]*(?:\([^)]*\)[^)]*)*)\)/);
      const boldMatch = remaining.match(/\*\*(.+?)\*\*/);
      const italicMatch = remaining.match(/(?<!\*)\*(?!\*)(.+?)(?<!\*)\*(?!\*)/);

      let earliestIndex = Infinity;
      let matchType = '';
      let matchResult: RegExpMatchArray | null = null;

      [{ m: codeMatch, t: 'code' }, { m: imgMatch, t: 'img' }, { m: linkMatch, t: 'link' }, { m: boldMatch, t: 'bold' }, { m: italicMatch, t: 'italic' }].forEach(({ m, t }) => {
        if (m && m.index !== undefined && m.index < earliestIndex) { earliestIndex = m.index; matchType = t; matchResult = m; }
      });

      if (!matchResult) { parts.push(<span key={keyIdx++}>{remaining}</span>); break; }
      const mr = matchResult! as RegExpMatchArray;

      if (earliestIndex > 0) parts.push(<span key={keyIdx++}>{remaining.slice(0, earliestIndex)}</span>);

      if (matchType === 'code') {
        parts.push(<code key={keyIdx++} style={{ background: 'hsla(0,0%,100%,0.08)', padding: '1px 5px', borderRadius: 3, fontFamily: "'Cascadia Code','Fira Code',monospace", fontSize: '0.9em', color: 'var(--primary)' }}>{mr[1]}</code>);
      } else if (matchType === 'img') {
        const url = resolveUrl(mr[2], source);
        const alt = mr[1];
        parts.push(
          <a key={keyIdx++} href={url} style={{ color: 'var(--primary)', textDecoration: 'none', fontSize: 'var(--font-size-sm)' }}>
            <img src={url} alt={alt} style={{ maxWidth: '100%', borderRadius: 4, display: 'block', margin: '4px 0', cursor: 'pointer' }}
              onError={(e) => {
                const img = e.target as HTMLImageElement;
                const parent = img.parentElement;
                if (parent) parent.textContent = alt || url;
              }} />
          </a>
        );
      } else if (matchType === 'link') {
        const url = resolveUrl(mr[2], source);
        parts.push(<a key={keyIdx++} href={url} style={{ color: 'var(--primary)', textDecoration: 'none' }}>{mr[1]}</a>);
      } else if (matchType === 'bold') {
        parts.push(<strong key={keyIdx++} style={{ fontWeight: 600 }}>{mr[1]}</strong>);
      } else if (matchType === 'italic') {
        parts.push(<em key={keyIdx++} style={{ fontStyle: 'italic' }}>{mr[1]}</em>);
      }

      remaining = remaining.slice(earliestIndex + mr[0].length);
    }
    return parts;
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)', height: '100%' }}>
      {/* Source tabs + search + sort */}
      <div style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', flexShrink: 0 }}>
        <div style={{ display: 'flex', gap: 4, flexShrink: 0 }}>
          <Button size="sm" variant={source === 'modrinth' ? 'primary' : 'ghost'} onClick={() => { setSource('modrinth'); setResults([]); setSelectedMod(null); setLoaded(false); }}>{t('mod.source_modrinth')}</Button>
          <Button size="sm" variant={source === 'curseforge' ? 'primary' : 'ghost'} onClick={() => { setSource('curseforge'); setResults([]); setSelectedMod(null); setLoaded(false); }}>{t('mod.source_curseforge')}</Button>
        </div>
        <div style={{ position: 'relative', flex: 1 }}>
          <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--text-tertiary)' }} />
          <input className="input" type="text" placeholder={t('mod.search_placeholder')} value={query} onChange={(e) => setQuery(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && search()} style={{ paddingLeft: 32 }} />
        </div>
        <Button onClick={search} loading={searching}>{t('common.search')}</Button>
      </div>

      {/* Sort controls */}
      <div style={{ display: 'flex', gap: 4, alignItems: 'center', flexShrink: 0 }}>
        <ArrowUpDown size={12} style={{ color: 'var(--text-tertiary)' }} />
        {(['downloads', 'newest', 'oldest'] as const).map((mode) => (
          <Button key={mode} size="sm" variant={sortMode === mode ? 'primary' : 'ghost'} onClick={() => setSortMode(mode)} style={{ fontSize: '11px', padding: '4px 8px' }}>
            {mode === 'downloads' && <><Star size={10} /> {t('content.sort_downloads')}</>}
            {mode === 'newest' && <><Calendar size={10} /> {t('content.sort_newest')}</>}
            {mode === 'oldest' && <><Calendar size={10} /> {t('content.sort_oldest')}</>}
          </Button>
        ))}
      </div>

      {/* Content */}
      <div style={{ display: 'flex', gap: 'var(--space-md)', flex: 1, overflow: 'hidden' }}>
        {/* Results list */}
        <div style={{ flex: selectedMod ? '0 0 38%' : '1', overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: 4 }}>
          {searching && (
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 40, color: 'var(--text-tertiary)' }}>
              <Loader2 size={24} style={{ animation: 'spin 1s linear infinite' }} />
            </div>
          )}
          {sortResults(results).map((mod) => {
            const isAdded = selectedMods.some((m) => String(m.modId) === String(mod.id));
            return (
              <div key={mod.id} onClick={() => selectMod(mod)} style={{
                display: 'flex', gap: 'var(--space-sm)', padding: '8px 10px', cursor: 'pointer',
                borderRadius: 'var(--radius-md)', border: '1px solid',
                borderColor: selectedMod?.id === mod.id ? 'var(--primary)' : isAdded ? 'var(--success)' : 'transparent',
                background: selectedMod?.id === mod.id ? 'var(--primary-dim)' : isAdded ? 'hsla(150, 60%, 50%, 0.08)' : 'var(--bg-secondary)',
                transition: 'all 0.15s', position: 'relative',
              }}>
                {getIcon(mod) && <img src={getIcon(mod)} alt="" style={{ width: 36, height: 36, borderRadius: 6, objectFit: 'cover', flexShrink: 0 }} />}
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', display: 'flex', alignItems: 'center', gap: 6 }}>
                    {mod.name}
                    {isAdded && <Check size={12} color="var(--success)" />}
                  </div>
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {mod.author || mod.authors?.[0]?.name}
                    {mod.downloads != null && ` · ${t('content.download_count', { n: formatDownloads(mod.downloads) })}`}
                  </div>
                </div>
                <Button size="sm" variant={isAdded ? 'ghost' : 'primary'} onClick={(e) => { e.stopPropagation(); addModLatest(mod); }} disabled={isAdded || addingModId === mod.id} style={{ flexShrink: 0, alignSelf: 'center' }}>
                  {addingModId === mod.id ? <Loader2 size={12} style={{ animation: 'spin 1s linear infinite' }} /> : isAdded ? <Check size={12} /> : t('content.add_btn')}
                </Button>
              </div>
            );
          })}
          {!searching && results.length === 0 && loaded && (
            <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>{t('mod.empty_results')}</div>
          )}
        </div>

        {/* Detail panel */}
        {selectedMod && (
          <div style={{ flex: 1, overflowY: 'auto', padding: 'var(--space-md)', background: 'var(--bg-secondary)', borderRadius: 'var(--radius-lg)', border: '1px solid var(--surface-border)' }}>
            {loadingDetail ? (
              <div style={{ display: 'flex', justifyContent: 'center', padding: 40 }}><Loader2 size={24} style={{ animation: 'spin 1s linear infinite' }} /></div>
            ) : (
              <>
                <div style={{ display: 'flex', gap: 'var(--space-md)', marginBottom: 'var(--space-md)' }}>
                  {getIcon(selectedMod) && <img src={getIcon(selectedMod)} alt="" style={{ width: 48, height: 48, borderRadius: 8, objectFit: 'cover' }} />}
                  <div style={{ flex: 1 }}>
                    <h3 style={{ margin: 0, fontSize: 'var(--font-size-lg)', fontWeight: 700 }}>{selectedMod.name}</h3>
                    <p style={{ margin: 0, fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>{selectedMod.description}</p>
                    {selectedMod.downloads != null && (
                      <p style={{ margin: '4px 0 0', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{formatDownloads(selectedMod.downloads)} downloads</p>
                    )}
                  </div>
                  <Button size="sm" variant={selectedMods.some((m) => String(m.modId) === String(selectedMod.id)) ? 'ghost' : 'primary'}
                    onClick={() => addModLatest(selectedMod)}
                    disabled={selectedMods.some((m) => String(m.modId) === String(selectedMod.id)) || addingModId === selectedMod.id}
                    style={{ flexShrink: 0, alignSelf: 'flex-start' }}>
                    {addingModId === selectedMod.id ? <Loader2 size={12} style={{ animation: 'spin 1s linear infinite' }} /> : selectedMods.some((m) => String(m.modId) === String(selectedMod.id)) ? <><Check size={12} /> {t('content.added_btn')}</> : t('content.add_btn')}
                  </Button>
                  <X size={16} style={{ cursor: 'pointer', color: 'var(--text-tertiary)', flexShrink: 0 }} onClick={() => setSelectedMod(null)} />
                </div>

                {/* Full description */}
                {modDetail?.body && (
                  <div style={{ marginBottom: 'var(--space-md)', padding: 'var(--space-md)', background: 'var(--bg-tertiary)', borderRadius: 'var(--radius-md)', fontSize: 'var(--font-size-sm)' }}
                    onClick={(e) => { const a = (e.target as HTMLElement).closest('a'); if (a?.href?.startsWith('http')) { e.preventDefault(); openUrl(a.href); } }}>
                    {renderMarkdown(modDetail.body, source)}
                  </div>
                )}
                {modDetail?.description && !modDetail.body && (
                  <div style={{ marginBottom: 'var(--space-md)', padding: 'var(--space-md)', background: 'var(--bg-tertiary)', borderRadius: 'var(--radius-md)', fontSize: 'var(--font-size-sm)' }}
                    onClick={(e) => { const a = (e.target as HTMLElement).closest('a'); if (a?.href?.startsWith('http')) { e.preventDefault(); openUrl(a.href); } }}>
                    {renderMarkdown(modDetail.description, source)}
                  </div>
                )}

                {/* Version selector */}
                {versions.length > 0 && (
                  <div style={{ marginBottom: 'var(--space-md)' }}>
                    <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 'var(--space-sm)' }}>
                      <span style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600 }}>{t('content.versions_label')}</span>
                      <span style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{t('content.versions_available', { n: versions.length.toString() })}</span>
                    </div>
                    <div style={{ maxHeight: 160, overflowY: 'auto', borderRadius: 'var(--radius-md)', border: '1px solid var(--surface-border)' }}>
                      {versions.slice(0, 20).map((v) => {
                        const file = v.files?.[0];
                        const isAdded = selectedMods.some((m) => m.filename === file?.filename);
                        return (
                          <div key={v.id} onClick={() => {
                            if (!isAdded && file) {
                              const newMod: SelectedMod = {
                                name: selectedMod!.name, iconUrl: getIcon(selectedMod!),
                                downloadUrl: file.url, filename: file.filename,
                                source: selectedMod!.source, modId: selectedMod!.id,
                                versionName: v.name || v.version_number,
                              };
                              setSelectedMods((prev) => [...prev, newMod]);
                              addToast(t('mod.added_toast', { name: selectedMod!.name }), 'success');
                            }
                          }}
                            style={{
                              display: 'flex', alignItems: 'center', justifyContent: 'space-between',
                              padding: '6px 10px', cursor: 'pointer', fontSize: 'var(--font-size-sm)',
                              background: isAdded ? 'hsla(150, 60%, 50%, 0.08)' : 'transparent',
                              borderBottom: '1px solid var(--surface-border)',
                            }}>
                            <div style={{ flex: 1, overflow: 'hidden' }}>
                              <span style={{ fontWeight: 500 }}>{v.name || v.version_number}</span>
                              {v.date_published && <span style={{ color: 'var(--text-tertiary)', marginLeft: 8, fontSize: 10 }}>{new Date(v.date_published).toLocaleDateString()}</span>}
                            </div>
                            {isAdded ? <Check size={12} color="var(--success)" /> : <span style={{ fontSize: 'var(--font-size-xs)', color: 'var(--primary)' }}>{t('content.add_btn')}</span>}
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

      {/* Bottom bar */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', padding: 'var(--space-sm) var(--space-md)', borderRadius: 'var(--radius-md)', background: 'var(--bg-secondary)', border: '1px solid var(--surface-border)', flexShrink: 0 }}>
        <span style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)', flexShrink: 0 }}>{t('common.items_selected', { n: selectedMods.length.toString() })}</span>
        <div style={{ flex: 1, display: 'flex', gap: 4, overflowX: 'auto' }}>
          {selectedMods.map((m, i) => (
            <div key={i} style={{ display: 'flex', alignItems: 'center', gap: 4, padding: '2px 8px', borderRadius: 999, background: m.isDependency ? 'var(--warning-dim)' : 'var(--primary-dim)', fontSize: 11, whiteSpace: 'nowrap', flexShrink: 0 }}>
              {m.name}
              <X size={10} style={{ cursor: 'pointer' }} onClick={() => removeMod(i)} />
            </div>
          ))}
        </div>
        <Button size="sm" variant="ghost" onClick={onCancel} style={{ flexShrink: 0 }}>{t('common.cancel')}</Button>
        <Button size="sm" onClick={() => onConfirm(selectedMods)} disabled={selectedMods.length === 0} style={{ flexShrink: 0 }}>
          {t('common.confirm')}
        </Button>
      </div>
    </div>
  );
}
