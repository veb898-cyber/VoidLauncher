import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

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

/** Requests already in flight, so two mounts never fetch the same list twice. */
const capesInflight = new Set<string>();

interface AccountsState {
  accounts: AccountEntry[];
  isLoading: boolean;
  error: string | null;
  /** Cape lists keyed by account id — the single source of truth for the UI. */
  capesByAccount: Record<string, CapesCacheEntry>;
  capesLoadingByAccount: Record<string, boolean>;
  capesErrorByAccount: Record<string, string | null>;
  loadAccounts: () => Promise<void>;
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

  loadAccounts: async () => {
    try {
      const accounts = await invoke<AccountEntry[]>('cmd_list_accounts');
      set({ accounts, error: null });
    } catch (e: any) {
      set({ error: e.toString() });
    }
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
      get().loadAccounts();
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
    const cached = get().capesByAccount[accountId];
    const fresh = cached && Date.now() - cached.fetchedAt < CAPES_TTL_MS;
    if (fresh && !force) return;
    if (capesInflight.has(accountId)) return;
    capesInflight.add(accountId);
    set((s) => ({
      capesLoadingByAccount: { ...s.capesLoadingByAccount, [accountId]: true },
      capesErrorByAccount: { ...s.capesErrorByAccount, [accountId]: null },
    }));
    try {
      const capes = await invoke<CapeEntry[]>('cmd_get_player_capes', { accountId });
      set((s) => ({
        capesByAccount: {
          ...s.capesByAccount,
          [accountId]: { capes, fetchedAt: Date.now() },
        },
      }));
    } catch (e: any) {
      set((s) => ({
        capesErrorByAccount: { ...s.capesErrorByAccount, [accountId]: String(e) },
      }));
    } finally {
      capesInflight.delete(accountId);
      set((s) => ({
        capesLoadingByAccount: { ...s.capesLoadingByAccount, [accountId]: false },
      }));
    }
  },

  applyCapes: (accountId: string, capes: CapeEntry[]) =>
    set((s) => ({
      capesByAccount: {
        ...s.capesByAccount,
        [accountId]: { capes, fetchedAt: Date.now() },
      },
    })),

  getPlayerCapes: (accountId: string) =>
    invoke<CapeEntry[]>('cmd_get_player_capes', { accountId }),

  setPlayerCape: (accountId: string, capeId: string | null) =>
    invoke<CapeEntry[]>('cmd_set_player_cape', { accountId, capeId }),
}));
