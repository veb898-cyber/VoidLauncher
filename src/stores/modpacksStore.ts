import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { addToast } from '../components/ui/Toast';
import { useInstanceStore } from './instanceStore';
import { useSettingsStore } from './settingsStore';
import { t } from '../lib/i18n';

export type ModpacksTab = 'modrinth' | 'curseforge' | 'atlauncher';

export interface MrHit {
  project_id: string;
  title: string;
  description: string;
  icon_url: string | null;
  downloads: number;
  slug: string;
  versions?: string[];
}

export interface CfHit {
  id: number;
  name: string;
  slug: string;
  summary: string;
  downloadCount: number;
  logo?: { thumbnailUrl?: string | null; url?: string | null } | null;
}

export interface AtlPack {
  id: number;
  name: string;
  safeName: string;
  description?: string | null;
  icon?: string | null;
  versions: { version: string; minecraft: string; isRecommended?: boolean; canUpdate?: boolean }[];
}

export interface AtlDetail {
  version: string;
  minecraft: string;
  loader?: string | null;
  loaderVersion?: string | null;
  mods: { name: string; version?: string | null }[];
  hasConfigs: boolean;
}

export interface MrVersion {
  id: string;
  project_id: string;
  name: string;
  version_number: string;
  game_versions: string[];
  files: { primary: boolean; filename: string; size: number }[];
}

export interface ProgressPayload {
  stage: string;
  current: number;
  total: number;
  message: string;
}

export interface FetchRetryPayload {
  source: string;
  attempt: number;
  total: number;
  message: string;
}

export interface FileProgressPayload {
  url: string;
  downloaded: number;
  total: number;
}

const PAGE_SIZE = 50;

interface ModpacksState {
  tab: ModpacksTab;
  query: string;
  mcFilter: string;
  loadingByTab: Record<ModpacksTab, boolean>;
  loadingMore: boolean;
  mrResults: MrHit[];
  cfResults: CfHit[];
  atlPacks: AtlPack[];
  atlFiltered: AtlPack[];
  cfOffset: number;
  cfHasMore: boolean;
  mrOffset: number;
  mrHasMore: boolean;

  selected: MrHit | CfHit | AtlPack | null;
  versions: any[];
  loadingDetail: boolean;
  atlDetail: AtlDetail | null;

  installName: string;
  nameError: boolean;
  installing: boolean;
  paused: boolean;
  pendingInstall: 'mr' | 'cf' | 'atl' | null;
  progress: ProgressPayload | null;
  installVersionId: string;
  installCfFileId: number | null;
  installAtlVersion: string;
  /** Where the current installName came from: 'user' (typed) or the pack id that auto-suggested it. */
  installNameSource: string | null;

  fetchRetry: FetchRetryPayload | null;
  fileProgress: FileProgressPayload | null;

switchTab: (tb: ModpacksTab) => void;
  loadInitial: (tb: ModpacksTab) => Promise<void>;
  setQuery: (q: string) => void;
  setMcFilter: (f: string) => void;
  setInstallName: (n: string) => void;
  setNameError: (v: boolean) => void;
  search: () => Promise<void>;
  loadMore: () => Promise<void>;
  selectMr: (hit: MrHit) => Promise<void>;
  selectCf: (hit: CfHit) => Promise<void>;
  selectAtl: (pack: AtlPack, version: string) => Promise<void>;
  selectAtlPack: (pack: AtlPack) => void;
  setInstallVersionId: (id: string) => void;
  setInstallCfFileId: (id: number | null) => void;
  setInstallAtlVersion: (v: string) => void;
  clearSelected: () => void;
  runInstall: (install: () => Promise<void>, kind: 'mr' | 'cf' | 'atl') => Promise<void>;
  pauseInstall: () => Promise<void>;
  resumeInstall: () => Promise<void>;
  installFromMr: () => Promise<void>;
  installFromCf: (hit: CfHit) => Promise<void>;
  installFromAtl: (pack: AtlPack) => Promise<void>;
  onFetchRetry: (payload: FetchRetryPayload) => void;
  onFileProgress: (payload: FileProgressPayload) => void;
  onImportProgress: (payload: ProgressPayload) => void;
  resetInstall: () => void;
}

function suggestInstanceName(base: string, existingNames: Set<string>): string {
  let name = base.replace(/[\\/:*?"<>|]/g, '').trim().slice(0, 64) || 'Modpack';
  if (!existingNames.has(name.toLowerCase())) return name;
  let i = 2;
  while (existingNames.has(`${name} (${i})`.toLowerCase())) i++;
  return `${name} (${i})`;
}

/**
 * Auto-fill the instance name field for a freshly selected pack. Keeps a
 * name the user typed by hand; re-suggests when the field is empty or still
 * carries the auto-suggestion of a *different* pack.
 */
function suggestNameFor(
  set: (partial: Partial<ModpacksState>) => void,
  get: () => ModpacksState,
  sourceId: string,
  title: string,
) {
  const st = get();
  const keepTyped = st.installNameSource === 'user';
  const samePack = st.installNameSource === sourceId;
  if (st.installName.trim() && (keepTyped || samePack)) return;
  const names = useInstanceStore.getState().instances.map((i) => i.name.toLowerCase());
  set({ installName: suggestInstanceName(title, new Set(names)), installNameSource: sourceId });
}

export const useModpacksStore = create<ModpacksState>()((set, get) => ({
  tab: 'modrinth',
  query: '',
  mcFilter: '',
  loadingByTab: { modrinth: false, curseforge: false, atlauncher: false },
  loadingMore: false,
  mrResults: [],
  cfResults: [],
  atlPacks: [],
  atlFiltered: [],
  cfOffset: 0,
  cfHasMore: false,
  mrOffset: 0,
  mrHasMore: false,

  selected: null,
  versions: [],
  loadingDetail: false,
  atlDetail: null,

  installName: '',
  nameError: false,
  installing: false,
  paused: false,
  pendingInstall: null,
  progress: null,
  installVersionId: '',
  installCfFileId: null,
  installAtlVersion: '',
  installNameSource: null,

  fetchRetry: null,
  fileProgress: null,

  switchTab: (tb) => {
    const s = get();
    if (s.tab === tb) return;
    set({
      tab: tb,
      selected: null,
      versions: [],
      atlDetail: null,
      installVersionId: '',
      installCfFileId: null,
      installAtlVersion: '',
      nameError: false,
    });
    // Load the catalog for the newly opened tab only if it has never
    // been fetched вЂ” results of other tabs stay cached in the store.
    const dataReady = tb === 'atlauncher'
      ? s.atlPacks.length > 0
      : tb === 'curseforge'
        ? s.cfResults.length > 0
        : s.mrResults.length > 0;
    if (!dataReady && !s.loadingByTab[tb]) {
      get().loadInitial(tb);
    }
  },

  setQuery: (q) => set({ query: q }),
  setMcFilter: (f) => set({ mcFilter: f }),
  setInstallName: (n) => set({ installName: n, installNameSource: n.trim() ? 'user' : null }),
  setNameError: (v) => set({ nameError: v }),

  loadInitial: async (tb: ModpacksTab) => {
    set((s) => ({ loadingByTab: { ...s.loadingByTab, [tb]: true }, selected: null, fetchRetry: null }));
    try {
      if (tb === 'atlauncher') {
        const packs = await invoke<AtlPack[]>('cmd_search_atlauncher');
        set({ atlPacks: packs, atlFiltered: packs });
      } else if (tb === 'curseforge') {
        const apiKey = useSettingsStore.getState().config?.curseforge_api_key;
        if (!apiKey) {
          set({ cfResults: [] });
          return;
        }
        const res = await invoke<any>('cmd_search_curseforge_modpacks', { query: '', mcVersion: null, loader: null, offset: 0, limit: PAGE_SIZE });
        const data: CfHit[] = res.data || [];
        set({ cfResults: data, cfOffset: data.length, cfHasMore: data.length >= PAGE_SIZE });
      } else {
        const res = await invoke<any>('cmd_search_modrinth_modpacks', { query: '', mcVersion: null, loader: null, index: 'downloads', offset: 0, limit: PAGE_SIZE });
        const hits: MrHit[] = res.hits || [];
        set({ mrResults: hits, mrOffset: hits.length, mrHasMore: hits.length >= PAGE_SIZE });
      }
    } catch (e: any) {
      addToast(t('modpacks.load_error', { error: e.toString() }), 'error');
    }
    set((s) => ({ loadingByTab: { ...s.loadingByTab, [tb]: false } }));
  },

  search: async () => {
    const { tab, query } = get();
    set((s) => ({ loadingByTab: { ...s.loadingByTab, [tab]: true }, selected: null, fetchRetry: null }));
    try {
      if (tab === 'curseforge') {
        const apiKey = useSettingsStore.getState().config?.curseforge_api_key;
        if (!apiKey) return;
        const res = await invoke<any>('cmd_search_curseforge_modpacks', { query, mcVersion: null, loader: null, offset: 0, limit: PAGE_SIZE });
        const data: CfHit[] = res.data || [];
        set({ cfResults: data, cfOffset: data.length, cfHasMore: data.length >= PAGE_SIZE });
      } else if (tab === 'modrinth') {
        const res = await invoke<any>('cmd_search_modrinth_modpacks', { query, mcVersion: null, loader: null, index: 'downloads', offset: 0, limit: PAGE_SIZE });
        const hits: MrHit[] = res.hits || [];
        set({ mrResults: hits, mrOffset: hits.length, mrHasMore: hits.length >= PAGE_SIZE });
      }
    } catch (e: any) {
      addToast(t('modpacks.search_error', { error: e.toString() }), 'error');
    }
    set((s) => ({ loadingByTab: { ...s.loadingByTab, [tab]: false } }));
  },

  loadMore: async () => {
    const s = get();
    const { tab } = s;
    if (s.loadingMore) return;
    const hasMore = tab === 'curseforge' ? s.cfHasMore : s.mrHasMore;
    if (!hasMore) return;
    const offset = tab === 'curseforge' ? s.cfOffset : s.mrOffset;
    set({ loadingMore: true });
    try {
      if (tab === 'curseforge') {
        const res = await invoke<any>('cmd_search_curseforge_modpacks', { query: s.query, mcVersion: null, loader: null, offset, limit: PAGE_SIZE });
        const fresh: CfHit[] = (res.data || []).filter((h: CfHit) => !s.cfResults.some((x) => x.id === h.id));
        set({ cfResults: [...s.cfResults, ...fresh], cfOffset: offset + fresh.length, cfHasMore: fresh.length >= PAGE_SIZE });
      } else if (tab === 'modrinth') {
        const res = await invoke<any>('cmd_search_modrinth_modpacks', { query: s.query, mcVersion: null, loader: null, index: 'downloads', offset, limit: PAGE_SIZE });
        const fresh: MrHit[] = (res.hits || []).filter((h: MrHit) => !s.mrResults.some((x) => x.project_id === h.project_id));
        set({ mrResults: [...s.mrResults, ...fresh], mrOffset: offset + fresh.length, mrHasMore: fresh.length >= PAGE_SIZE });
      }
    } catch { }
    set({ loadingMore: false });
  },

  selectMr: async (hit) => {
    const sourceId = `mr:${hit.project_id}`;
    set({
      selected: hit, atlDetail: null, versions: [], loadingDetail: true, nameError: false,
      installVersionId: '', installCfFileId: null, installAtlVersion: '',
    });
    suggestNameFor(set, get, sourceId, hit.title);
    try {
      const vers = await invoke<MrVersion[]>('cmd_get_modrinth_modpack_versions', { projectId: hit.project_id });
      set({ versions: vers });
    } catch (e: any) {
      addToast(t('modpacks.detail_error', { error: e.toString() }), 'error');
    }
    set({ loadingDetail: false });
  },

  selectCf: async (hit) => {
    const st = get();
    const sourceId = `cf:${hit.id}`;
    set({
      selected: hit, atlDetail: null, versions: [], loadingDetail: true, nameError: false,
      installVersionId: '', installCfFileId: null, installAtlVersion: '',
    });
    suggestNameFor(set, get, sourceId, hit.name);
    try {
      const res = await invoke<any>('cmd_get_curseforge_modpack_files', { modId: hit.id, mcVersion: st.mcFilter || null, loader: null });
      set({ versions: res.data || [] });
    } catch (e: any) {
      addToast(t('modpacks.detail_error', { error: e.toString() }), 'error');
    }
    set({ loadingDetail: false });
  },

  selectAtl: async (pack, version) => {
    set({ selected: pack, atlDetail: null, loadingDetail: true, nameError: false });
    try {
      const detail = await invoke<AtlDetail>('cmd_get_atlauncher_version_detail', { safeName: pack.safeName, version });
      set({ atlDetail: detail });
    } catch (e: any) {
      addToast(t('modpacks.detail_error', { error: e.toString() }), 'error');
    }
    set({ loadingDetail: false });
  },

  selectAtlPack: (pack) => {
    const sourceId = `atl:${pack.id}`;
    set({
      selected: pack, atlDetail: null, nameError: false,
      installVersionId: '', installCfFileId: null, installAtlVersion: '',
    });
    suggestNameFor(set, get, sourceId, pack.name);
    const rec = pack.versions.find((v) => v.isRecommended) ?? pack.versions[0];
    set({ versions: pack.versions });
    if (rec) {
      set({ installAtlVersion: rec.version });
      get().selectAtl(pack, rec.version);
    }
  },

  setInstallVersionId: (id) => set({ installVersionId: id }),
  setInstallCfFileId: (id) => set({ installCfFileId: id }),
  setInstallAtlVersion: (v) => set({ installAtlVersion: v }),
  clearSelected: () => set({ selected: null }),

  runInstall: async (install, kind) => {
    const name = get().installName.trim();
    if (!name) {
      set({ nameError: true });
      return;
    }
    set({ nameError: false, installing: true, paused: false, progress: null, fileProgress: null, pendingInstall: kind });
    try {
      await install();
      await useInstanceStore.getState().loadInstances();
      addToast(t('modpacks.installed'), 'success');
    } catch (e: any) {
      if (typeof e === 'string' && e.includes('paused')) {
        set({ installing: false, paused: true, progress: null, fileProgress: null });
        return;
      }
      addToast(t('modpacks.install_error', { error: e.toString() }), 'error');
    }
    set({ installing: false, progress: null, fileProgress: null, pendingInstall: null, paused: false });
  },

  pauseInstall: async () => {
    // Optimistic UI: flip to "paused" immediately so the user sees feedback
    // even while the backend is still finishing the current chunk/retry wait.
    set({ paused: true });
    try { await invoke('cmd_pause_modpack_install'); } catch { }
  },

  resumeInstall: async () => {
    await invoke('cmd_resume_modpack_install').catch(() => {});
    // Wait up to 5s for the old install command to notice the resume (it may
    // have already stopped — then it returns "paused" and we restart below).
    const deadline = Date.now() + 5000;
    while (get().installing && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 100));
    }
    set({ paused: false });
    if (get().installing) return;
    const kind = get().pendingInstall;
    const sel = get().selected;
    if (kind === 'cf' && sel && 'downloadCount' in sel) {
      await get().installFromCf(sel as CfHit);
    } else if (kind === 'atl' && sel && 'safeName' in sel) {
      await get().installFromAtl(sel as AtlPack);
    } else if (kind === 'mr') {
      await get().installFromMr();
    }
  },

  installFromMr: async () => {
    const v = get().versions.find((x) => x.id === get().installVersionId) as MrVersion | undefined;
    if (!v) return;
    await get().runInstall(async () => {
      await invoke('cmd_install_modrinth_modpack', { versionId: v.id, instanceName: get().installName.trim() });
    }, 'mr');
  },

  installFromCf: async (hit) => {
    const { installCfFileId } = get();
    if (installCfFileId == null) return;
    await get().runInstall(async () => {
      await invoke('cmd_install_curseforge_modpack', { modId: hit.id, fileId: installCfFileId, instanceName: get().installName.trim() });
    }, 'cf');
  },

  installFromAtl: async (pack) => {
    const { installAtlVersion } = get();
    if (!installAtlVersion) return;
    await get().runInstall(async () => {
      await invoke('cmd_install_atlauncher_modpack', { packId: pack.id, version: installAtlVersion, instanceName: get().installName.trim() });
    }, 'atl');
  },

  onFetchRetry: (payload) => set({ fetchRetry: payload }),
  onFileProgress: (payload) => set({ fileProgress: payload }),
  onImportProgress: (payload) => {
    set({ progress: payload, fileProgress: null });
    if (payload.stage === 'done') {
      set({ installing: false, progress: null, fileProgress: null, pendingInstall: null });
    }
  },
resetInstall: () => set({ installing: false, progress: null, fileProgress: null, pendingInstall: null, paused: false }),
}));