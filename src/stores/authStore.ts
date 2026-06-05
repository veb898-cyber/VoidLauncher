import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

interface MinecraftProfile {
  id: string;
  name: string;
}

interface AuthState {
  profile: MinecraftProfile | null;
  isLoggedIn: boolean;
  isLoading: boolean;
  error: string | null;
  deviceCode: string | null;
  userCode: string | null;
  verificationUri: string | null;

  checkAuth: () => Promise<void>;
  startLogin: () => Promise<void>;
  pollLogin: () => Promise<void>;
  logout: () => Promise<void>;
  clearError: () => void;
}

export const useAuthStore = create<AuthState>((set, get) => ({
  profile: null,
  isLoggedIn: false,
  isLoading: false,
  error: null,
  deviceCode: null,
  userCode: null,
  verificationUri: null,

  checkAuth: async () => {
    try {
      const state = await invoke<any>('cmd_get_auth_state');
      if (state.profile) {
        set({
          profile: state.profile,
          isLoggedIn: true,
        });
      }
    } catch (e) {
      console.error('Auth check failed:', e);
    }
  },

  startLogin: async () => {
    set({ isLoading: true, error: null });
    try {
      const response = await invoke<any>('cmd_start_login');
      set({
        deviceCode: response.device_code,
        userCode: response.user_code,
        verificationUri: response.verification_uri,
        isLoading: false,
      });
    } catch (e: any) {
      set({ error: e.toString(), isLoading: false });
    }
  },

  pollLogin: async () => {
    const { deviceCode } = get();
    if (!deviceCode) return;

    set({ isLoading: true });
    try {
      const profile = await invoke<MinecraftProfile>('cmd_poll_login', {
        deviceCode,
      });
      set({
        profile,
        isLoggedIn: true,
        isLoading: false,
        deviceCode: null,
        userCode: null,
        verificationUri: null,
      });
    } catch (e: any) {
      const errMsg = e.toString();
      if (errMsg.includes('authorization_pending')) {
        set({ isLoading: false });
        // Still waiting, don't set error
      } else {
        set({ error: errMsg, isLoading: false });
      }
    }
  },

  logout: async () => {
    try {
      await invoke('cmd_logout');
      set({
        profile: null,
        isLoggedIn: false,
        deviceCode: null,
        userCode: null,
        verificationUri: null,
      });
    } catch (e: any) {
      set({ error: e.toString() });
    }
  },

  clearError: () => set({ error: null }),
}));
