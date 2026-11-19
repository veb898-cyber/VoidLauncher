import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

/** Tailscale status nested in the room snapshot (serialized snake_case by Rust). */
export interface TailscaleStatus {
  installed: boolean;
  service_ok: boolean;
  logged_in: boolean;
  version: string | null;
  tailnet: string | null;
  login_name: string | null;
  self_ip: string | null;
  auth_url: string | null;
}

/** A peer machine on the tailnet (host candidate); serialized snake_case. */
export interface RoomHostInfo {
  node_key: string;
  host_name: string;
  dns_name: string;
  tailnet_ips: string[];
  online: boolean;
}

export type RoomRole = 'host' | 'guest' | 'none';

/** Full room snapshot returned by the backend (serialized camelCase). */
export interface RoomStatus {
  role: RoomRole;
  roomId: string | null;
  roomName: string | null;
  tailscale: TailscaleStatus;
  host: RoomHostInfo | null;
  minecraftPort: number | null;
  bridgePort: number;
  endpoint: string | null;
  hostOnline: boolean;
  /** Registered host instance (local name); never the invitation secret. */
  hostInstance: string | null;
}

/** Progress payload of the `room_progress` event. */
export interface RoomProgress {
  stage: string;
  fraction: number;
  message: string;
}

interface RoomStore {
  status: RoomStatus | null;
  /** Host-only copyable invitation (room id + secret); never part of status. */
  invitation: string | null;
  progress: RoomProgress | null;
  busy: boolean;
  error: string | null;
  loaded: boolean;

  loadStatus: () => Promise<void>;
  loadInvitation: () => Promise<void>;
  /** Refresh the Tailscale snapshot; never installs or signs in. */
  checkLink: () => Promise<void>;
  /** Install Tailscale after the user explicitly confirmed it. */
  installTailscale: () => Promise<void>;
  /** Start the interactive Tailscale sign-in after an explicit click. */
  login: () => Promise<void>;
  createHost: (roomName: string) => Promise<void>;
  joinGuest: (roomCode: string) => Promise<void>;
  leave: () => Promise<void>;
  refresh: () => Promise<void>;
  detectMinecraftPort: (instanceName: string) => Promise<number | null>;
  reportMinecraftPort: (port: number | null) => Promise<void>;
  setHostInstance: (instanceName: string) => Promise<boolean>;
  openAdminConsole: () => Promise<void>;
  setProgress: (p: RoomProgress | null) => void;
  clearError: () => void;
}

export const useRoomStore = create<RoomStore>((set, get) => ({
  status: null,
  invitation: null,
  progress: null,
  busy: false,
  error: null,
  loaded: false,

  loadStatus: async () => {
    try {
      const status = await invoke<RoomStatus>('cmd_room_status');
      set((state) => ({
        status,
        loaded: true,
        error: null,
        invitation: status.role === 'host' ? state.invitation : null,
      }));
    } catch (e: any) {
      set({ loaded: true, error: e.toString() });
    }
  },

  loadInvitation: async () => {
    try {
      const invitation = await invoke<string>('cmd_room_invitation');
      set({ invitation });
    } catch {
      // Not the host (or Rooms just turned off); no invitation to copy.
      set({ invitation: null });
    }
  },

  checkLink: async () => {
    set({ busy: true, error: null });
    try {
      const status = await invoke<RoomStatus>('cmd_room_check_link');
      set({ status, busy: false });
    } catch (e: any) {
      set({ busy: false, error: e.toString() });
    }
  },

  installTailscale: async () => {
    set({ busy: true, error: null });
    try {
      const status = await invoke<RoomStatus>('cmd_room_install_tailscale');
      set({ status, busy: false });
    } catch (e: any) {
      set({ busy: false, error: e.toString() });
    }
  },

  login: async () => {
    set({ busy: true, error: null });
    try {
      const status = await invoke<RoomStatus>('cmd_room_login');
      set({ status, busy: false });
    } catch (e: any) {
      set({ busy: false, error: e.toString() });
    }
  },

  createHost: async (roomName: string) => {
    set({ busy: true, error: null });
    try {
      const status = await invoke<RoomStatus>('cmd_room_create_host', {
        roomName: roomName.trim() || null,
      });
      set({ status, busy: false, invitation: null });
    } catch (e: any) {
      set({ busy: false, error: e.toString() });
    }
  },

  joinGuest: async (roomCode: string) => {
    set({ busy: true, error: null });
    try {
      const status = await invoke<RoomStatus>('cmd_room_join_guest', {
        roomCode: roomCode.trim(),
      });
      set({ status, busy: false, invitation: null });
    } catch (e: any) {
      set({ busy: false, error: e.toString() });
    }
  },

  leave: async () => {
    set({ busy: true, error: null });
    try {
      const status = await invoke<RoomStatus>('cmd_room_leave');
      set({ status, busy: false, progress: null, invitation: null });
    } catch (e: any) {
      set({ busy: false, error: e.toString() });
    }
  },

  refresh: async () => {
    try {
      const status = await invoke<RoomStatus>('cmd_room_refresh');
      set({ status });
    } catch (e: any) {
      set({ error: e.toString() });
    }
  },

  detectMinecraftPort: async (instanceName: string) => {
    try {
      const port = await invoke<number | null>('cmd_room_detect_minecraft_port', {
        instanceName,
      });
      const current = get().status?.minecraftPort ?? null;
      // The host view polls while the game runs to notice a closed LAN world.
      // Only write/refresh when the detected port actually changed.
      if ((port ?? null) !== current) {
        await invoke('cmd_room_report_minecraft_port', { port: port ?? null });
        await get().loadStatus();
      }
      return port ?? null;
    } catch (e: any) {
      set({ error: e.toString() });
      return null;
    }
  },

  reportMinecraftPort: async (port: number | null) => {
    try {
      await invoke('cmd_room_report_minecraft_port', { port });
      await get().loadStatus();
    } catch (e: any) {
      set({ error: e.toString() });
    }
  },

  setHostInstance: async (instanceName: string) => {
    try {
      await invoke('cmd_room_set_host_instance', { instance: instanceName });
      return true;
    } catch (e: any) {
      set({ error: e.toString() });
      return false;
    }
  },

  openAdminConsole: async () => {
    try {
      await invoke('cmd_room_open_admin_console');
    } catch (e: any) {
      set({ error: e.toString() });
    }
  },

  setProgress: (progress) => set({ progress }),
  clearError: () => set({ error: null }),
}));
