import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ServerStatusPoller, type PingResult, type ServerStatus } from '../lib/serverStatus';

/**
 * Live statuses of one instance's saved servers, keyed by the saved address
 * string. The poller checks on mount, whenever the address list changes and
 * every 60 s while the tab is open; `refresh()` forces a re-check of all.
 */
export function useServerStatuses(instanceName: string, addresses: string[]) {
  const [statuses, setStatuses] = useState<Record<string, ServerStatus>>({});
  const pollerRef = useRef<ServerStatusPoller | null>(null);

  // The poller is bound to the instance; switching instances resets state.
  useEffect(() => {
    setStatuses({});
    const poller = new ServerStatusPoller({
      ping: (address) => invoke<PingResult>('cmd_ping_server', { instanceName, address }),
      onUpdate: (address, status) => {
        setStatuses((previous) => {
          if (status === null) {
            if (!(address in previous)) return previous;
            const next = { ...previous };
            delete next[address];
            return next;
          }
          return { ...previous, [address]: status };
        });
      },
      concurrency: 4,
      intervalMs: 60_000,
    });
    pollerRef.current = poller;
    return () => {
      poller.destroy();
      pollerRef.current = null;
    };
  }, [instanceName]);

  // Address-list changes are forwarded by value so re-renders with an equal
  // list do not churn the poller. The instance is part of the deps because a
  // fresh poller (recreated above) has no addresses yet.
  const addressKey = addresses.join('\n');
  useEffect(() => {
    pollerRef.current?.setAddresses(addressKey ? addressKey.split('\n') : []);
  }, [addressKey, instanceName]);

  const refresh = useMemo(() => () => pollerRef.current?.refresh(), []);
  return { statuses, refresh };
}
