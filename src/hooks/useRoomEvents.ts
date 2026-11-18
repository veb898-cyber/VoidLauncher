import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useRoomStore } from '../stores/roomStore';
import type { RoomProgress } from '../stores/roomStore';
import { addToast } from '../components/ui/Toast';

/**
 * Subscribe the Rooms page to room events.
 *
 * * `room_progress` — Tailscale install / sign-in progress (drives the
 *   progress bar in the setup view).
 * * `room_error` — background workflow failure (e.g. Tailscale install) →
 *   shown as a global toast, same as every other launcher error.
 * * `room_status_changed` — the room snapshot changed (peer discovery found
 *   the host, the host advertised a port, …); we simply reload the snapshot.
 *
 * Uses the same race-safe listener pattern as `useGameEvents`: `listen()` is
 * async and the promise may resolve after unmount, so late listeners are
 * detached immediately.
 */
interface RoomErrorPayload {
  message: string;
}

export function useRoomEvents() {
  const setProgress = useRoomStore((s) => s.setProgress);
  const loadStatus = useRoomStore((s) => s.loadStatus);

  useEffect(() => {
    let cancelled = false;
    const unlisteners: Array<() => void> = [];

    const register = (promise: Promise<() => void>) => {
      promise
        .then((fn) => {
          if (cancelled) {
            try { fn(); } catch { /* ignore */ }
            return;
          }
          unlisteners.push(fn);
        })
        .catch(() => { /* ignore */ });
    };

    register(listen<RoomProgress>('room_progress', (event) => {
      const payload = event.payload;
      // A finished install/login leaves the setup card on its next step; the
      // completed progress bar must not sit at 100% forever.
      const finished = (payload.stage === 'install' || payload.stage === 'login') && payload.fraction >= 1;
      setProgress(finished ? null : payload);
    }));

    register(listen<RoomErrorPayload>('room_error', (event) => {
      // A failed background step (install/login) must not leave the progress
      // bar frozen at its last fraction.
      setProgress(null);
      addToast(event.payload.message, 'error');
    }));

    register(listen('room_status_changed', () => {
      loadStatus();
    }));

    return () => {
      cancelled = true;
      const snapshot = unlisteners.slice();
      unlisteners.length = 0;
      for (const fn of snapshot) {
        try { fn(); } catch { /* ignore */ }
      }
    };
  }, [setProgress, loadStatus]);
}
