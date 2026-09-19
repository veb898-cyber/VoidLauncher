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
  discoveryError: string | null;
}

/** Progress payload of the `room_progress` event. */
export interface RoomProgress {
  stage: string;
  fraction: number;
  message: string;
}

interface RoomStore {
  status: RoomStatus | null;
  progress: RoomProgress | null;
  detectedPort: number | null;
  busy: boolean;
  error: string | null;
  loaded: boolean;

  loadStatus: () => Promise<void>;
  checkLink: () => Promise<void>;
  createHost: (roomName: string) => Promise<void>;
  joinGuest: (roomCode: string) => Promise<void>;
  leave: () => Promise<void>;
  refresh: () => Promise<void>;
  detectMinecraftPort: () => Promise<number | null>;
  reportMinecraftPort: (port: number | null) => Promise<void>;
  openAdminConsole: () => Promise<void>;
  setProgress: (p: RoomProgress | null) => void;
  clearError: () => void;
}

export const useRoomStore = create<RoomStore>((set, get) => ({
  status: null,
  progress: null,
  detectedPort: null,
  busy: false,
  error: null,
  loaded: false,

  loadStatus: async () => {
    try {
      const status = await invoke<RoomStatus>('cmd_room_status');
      set({ status, loaded: true, error: null });
    } catch (e: any) {
      set({ loaded: true, error: e.toString() });
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

  createHost: async (roomName: string) => {
    set({ busy: true, error: null });
    try {
      const status = await invoke<RoomStatus>('cmd_room_create_host', {
        roomName: roomName.trim() || null,
      });
      set({ status, busy: false, detectedPort: null });
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
      set({ status, busy: false, detectedPort: null });
    } catch (e: any) {
      set({ busy: false, error: e.toString() });
    }
  },

  leave: async () => {
    set({ busy: true, error: null });
    try {
      const status = await invoke<RoomStatus>('cmd_room_leave');
      set({ status, busy: false, progress: null, detectedPort: null });
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

  detectMinecraftPort: async () => {
    try {
      const port = await invoke<number | null>('cmd_room_detect_minecraft_port');
      set({ detectedPort: port ?? null });
      if (port && port > 0) {
        await invoke('cmd_room_report_minecraft_port', { port });
      }
      await get().loadStatus();
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
