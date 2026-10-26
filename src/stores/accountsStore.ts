import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { logUiMessage } from '../lib/uiLog';

/**
 * Public, token-free account metadata. The backend strips `access_token` and
 * `elyby_token` at the bridge — they live only on disk in `accounts.json` and
 * are read by Rust at launch time. Do NOT add token fields here; if a feature
 * needs the token, add a dedicated `cmd_get_launch_credentials(id)` command
 * that returns the token directly to the call site without round-tripping
 * through the store.
 */
export interface AccountEntry {
  id: string;
  name: string;
  account_type: 'Microsoft' | 'Offline' | 'ElyBy';
  uuid?: string;
  skin_variant?: string;
  default: boolean;
  /** Microsoft only: whether a stored session exists in the OS vault. */
  has_ms_session?: boolean;
}

/** One cape owned by a Microsoft account (png = texture as a data URL). */
export interface CapeEntry {
  id: string;
  alias: string;
  active: boolean;
  png: string;
}

/** One cached cape list plus the moment it was fetched. */
export interface CapesCacheEntry {
  capes: CapeEntry[];
  fetchedAt: number;
}

/**
 * How long a cached cape list is considered fresh. Re-opening the page inside
 * this window answers from memory without a network round-trip; after it the
 * list is still shown immediately and refreshed in the background.
 */
const CAPES_TTL_MS = 45_000;

/**
 * Last successful cape list per account, persisted across app restarts. The
 * cape is part of the character's look, so it must be on the 3D model from the
 * first frame — exactly like the cached skin — instead of popping in after the
 * profile request and texture downloads finish. The list is still refreshed
 * from the network in the background; this is only the instant first paint.
 */
const CAPES_STORAGE_PREFIX = 'voidlauncher-capes:';

function readPersistedCapes(accountId: string): CapeEntry[] | null {
  try {
    const raw = localStorage.getItem(CAPES_STORAGE_PREFIX + accountId);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    return parsed.filter((c): c is CapeEntry => {
      if (!c || typeof c !== 'object') return false;
      const entry = c as Partial<CapeEntry>;
      return (
        typeof entry.id === 'string' &&
        typeof entry.alias === 'string' &&
        typeof entry.active === 'boolean' &&
        typeof entry.png === 'string'
      );
    });
  } catch {
    return null;
  }
}

function persistCapes(accountId: string, capes: CapeEntry[]): void {
  try {
    localStorage.setItem(CAPES_STORAGE_PREFIX + accountId, JSON.stringify(capes));
  } catch {
    // Storage may be unavailable or full; the in-memory cache still works.
  }
}

function forgetPersistedCapes(accountId: string): void {
  try {
    localStorage.removeItem(CAPES_STORAGE_PREFIX + accountId);
  } catch {
    // Nothing to clean up if storage is unavailable.
  }
}

/**
 * Keep textures we already hold when the backend answers with metadata only
 * (a cape switch) or when a texture download failed. A non-empty PNG always
 * wins, so a genuinely updated texture still replaces the old one.
 */
function withPreservedPngs(next: CapeEntry[], previous: CapeEntry[] | undefined): CapeEntry[] {
  if (!previous || previous.length === 0) return next;
  const pngById = new Map(previous.map((c) => [c.id, c.png]));
  return next.map((c) => (c.png ? c : { ...c, png: pngById.get(c.id) || '' }));
}

/** Requests already in flight, so two mounts never fetch the same list twice. */
const capesInflight = new Set<string>();

/**
 * The Sidebar, Home, Accounts and Settings all ask for the account list during
 * startup. Coalesce those calls and answer repeat mounts from a very short
 * cache; account mutations update the store directly, so this only removes
 * duplicate IPC requests, not freshness.
 */
const ACCOUNTS_TTL_MS = 2_000;
let accountsInflight: Promise<void> | null = null;
let accountsLoadedAt = 0;

interface AccountsState {
  accounts: AccountEntry[];
  isLoading: boolean;
  error: string | null;
  /** Cape lists keyed by account id — the single source of truth for the UI. */
  capesByAccount: Record<string, CapesCacheEntry>;
  capesLoadingByAccount: Record<string, boolean>;
  capesErrorByAccount: Record<string, string | null>;
  loadAccounts: (force?: boolean) => Promise<void>;
  addOfflineAccount: (username: string) => Promise<void>;
  addElybyAccount: (username: string, password: string) => Promise<void>;
  removeAccount: (id: string) => Promise<void>;
  setDefaultAccount: (id: string) => Promise<void>;
  changeSkin: (accountId: string, skinPath: string, variant?: string) => Promise<void>;
  /** Load (or silently refresh) the cape list of one account. */
  fetchCapes: (accountId: string, force?: boolean) => Promise<void>;
  /** Replace the cached cape list (optimistic updates and server answers). */
  applyCapes: (accountId: string, capes: CapeEntry[]) => void;
  getPlayerCapes: (accountId: string) => Promise<CapeEntry[]>;
  setPlayerCape: (accountId: string, capeId: string | null) => Promise<CapeEntry[]>;
}

export const useAccountsStore = create<AccountsState>((set, get) => ({
  accounts: [],
  isLoading: false,
  error: null,
  capesByAccount: {},
  capesLoadingByAccount: {},
  capesErrorByAccount: {},

  loadAccounts: async (force = false) => {
    if (
      !force &&
      accountsLoadedAt > 0 &&
      Date.now() - accountsLoadedAt < ACCOUNTS_TTL_MS
    ) {
      return;
    }
    if (accountsInflight) return accountsInflight;
    accountsInflight = (async () => {
      try {
        const accounts = await invoke<AccountEntry[]>('cmd_list_accounts');
        accountsLoadedAt = Date.now();
        // Hydrate the persisted cape lists before the page renders, so the
        // worn cape is already on the 3D model instead of arriving with the
        // profile request. In-memory entries (this session) stay authoritative.
        const persisted: Record<string, CapesCacheEntry> = {};
        for (const account of accounts) {
          if (account.account_type !== 'Microsoft') continue;
          const capes = readPersistedCapes(account.id);
          if (capes) persisted[account.id] = { capes, fetchedAt: 0 };
        }
        set((s) => ({
          accounts,
          error: null,
          capesByAccount: { ...persisted, ...s.capesByAccount },
        }));
      } catch (e: any) {
        set({ error: e.toString() });
      } finally {
        accountsInflight = null;
      }
    })();
    return accountsInflight;
  },

  addOfflineAccount: async (username: string) => {
    set({ isLoading: true, error: null });
    try {
      const accounts = await invoke<AccountEntry[]>('cmd_add_offline_account', { username });
      set({ accounts, isLoading: false });
    } catch (e: any) {
      set({ error: e.toString(), isLoading: false });
      throw e;
    }
  },

  addElybyAccount: async (username: string, password: string) => {
    set({ isLoading: true, error: null });
    try {
      const accounts = await invoke<AccountEntry[]>('cmd_add_elyby_account', { username, password });
      set({ accounts, isLoading: false });
    } catch (e: any) {
      set({ error: e.toString(), isLoading: false });
      throw e;
    }
  },

  removeAccount: async (id: string) => {
    try {
      const accounts = await invoke<AccountEntry[]>('cmd_remove_account', { id });
      // Drop every trace of the removed account so a future account that
      // reuses the id never shows stale capes.
      forgetPersistedCapes(id);
      set((s) => {
        const capesByAccount = { ...s.capesByAccount };
        const capesLoadingByAccount = { ...s.capesLoadingByAccount };
        const capesErrorByAccount = { ...s.capesErrorByAccount };
        delete capesByAccount[id];
        delete capesLoadingByAccount[id];
        delete capesErrorByAccount[id];
        return { accounts, capesByAccount, capesLoadingByAccount, capesErrorByAccount };
      });
    } catch (e: any) {
      set({ error: e.toString() });
      throw e;
    }
  },

  setDefaultAccount: async (id: string) => {
    try {
      const accounts = await invoke<AccountEntry[]>('cmd_set_default_account', { id });
      set({ accounts });
    } catch (e: any) {
      set({ error: e.toString() });
    }
  },

  changeSkin: async (accountId: string, skinPath: string, variant = 'classic') => {
    try {
      await invoke('cmd_change_skin', { accountId, skinPath, variant });
      get().loadAccounts(true);
    } catch (e: any) {
      set({ error: e.toString() });
      throw e;
    }
  },

  /**
   * Load the cape list of one account into the shared cache.
   *
   * The cached list is never cleared while a refresh is running: the picker
   * and the 3D preview keep rendering the previous (still valid) data and are
   * swapped only when the fresh answer arrives. A failed refresh keeps the
   * last good list and reports the error next to it.
   */
  fetchCapes: async (accountId: string, force = false) => {
    let cached = get().capesByAccount[accountId];
    if (!cached) {
      // First visit in this session: paint the persisted list immediately,
      // then refresh it from the profile endpoint below (fetchedAt = 0 marks
      // it as stale on purpose, so the background refresh always runs).
      const persisted = readPersistedCapes(accountId);
      if (persisted) {
        cached = { capes: persisted, fetchedAt: 0 };
        set((s) => ({
          capesByAccount: { ...s.capesByAccount, [accountId]: cached! },
        }));
      }
    }
    const fresh = cached && Date.now() - cached.fetchedAt < CAPES_TTL_MS;
    if (fresh && !force) return;
    if (capesInflight.has(accountId)) return;
    capesInflight.add(accountId);
    set((s) => ({
      capesLoadingByAccount: { ...s.capesLoadingByAccount, [accountId]: true },
      capesErrorByAccount: { ...s.capesErrorByAccount, [accountId]: null },
    }));
    try {
      const capes = await invoke<CapeEntry[]>('cmd_get_player_capes', { accountId, force });
      const merged = withPreservedPngs(capes, get().capesByAccount[accountId]?.capes);
      persistCapes(accountId, merged);
      set((s) => ({
        capesByAccount: {
          ...s.capesByAccount,
          [accountId]: { capes: merged, fetchedAt: Date.now() },
        },
      }));
    } catch (e: any) {
      const message = String(e);
      // The inline picker shows this too, but the user explicitly expects
      // cape failures in the launcher log as well.
      logUiMessage(`Could not load capes: ${message}`, 'error', 'accounts');
      set((s) => ({
        capesErrorByAccount: { ...s.capesErrorByAccount, [accountId]: message },
      }));
    } finally {
      capesInflight.delete(accountId);
      set((s) => ({
        capesLoadingByAccount: { ...s.capesLoadingByAccount, [accountId]: false },
      }));
    }
  },

  applyCapes: (accountId: string, capes: CapeEntry[]) => {
    const merged = withPreservedPngs(capes, get().capesByAccount[accountId]?.capes);
    persistCapes(accountId, merged);
    set((s) => ({
      capesByAccount: {
        ...s.capesByAccount,
        [accountId]: { capes: merged, fetchedAt: Date.now() },
      },
    }));
  },

  getPlayerCapes: (accountId: string) =>
    invoke<CapeEntry[]>('cmd_get_player_capes', { accountId }),

  setPlayerCape: (accountId: string, capeId: string | null) =>
    invoke<CapeEntry[]>('cmd_set_player_cape', { accountId, capeId }),
}));
