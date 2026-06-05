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
}

interface AccountsState {
  accounts: AccountEntry[];
  isLoading: boolean;
  error: string | null;
  loadAccounts: () => Promise<void>;
  addOfflineAccount: (username: string) => Promise<void>;
  addElybyAccount: (username: string, password: string) => Promise<void>;
  removeAccount: (id: string) => Promise<void>;
  setDefaultAccount: (id: string) => Promise<void>;
  changeSkin: (accountId: string, skinPath: string, variant?: string) => Promise<void>;
}

export const useAccountsStore = create<AccountsState>((set, get) => ({
  accounts: [],
  isLoading: false,
  error: null,

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
    }
  },

  addElybyAccount: async (username: string, password: string) => {
    set({ isLoading: true, error: null });
    try {
      const accounts = await invoke<AccountEntry[]>('cmd_add_elyby_account', { username, password });
      set({ accounts, isLoading: false });
    } catch (e: any) {
      set({ error: e.toString(), isLoading: false });
    }
  },

  removeAccount: async (id: string) => {
    try {
      const accounts = await invoke<AccountEntry[]>('cmd_remove_account', { id });
      set({ accounts });
    } catch (e: any) {
      set({ error: e.toString() });
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
    }
  },
}));
