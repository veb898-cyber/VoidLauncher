import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useInstanceStore } from './instanceStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const invokeMock = vi.mocked(invoke);

function resetStore() {
  useInstanceStore.setState({
    instances: [],
    selectedInstance: null,
    isLoading: false,
    error: null,
    isLaunching: false,
    launchStatus: null,
    elybySkinsPrompt: null,
  });
}

async function waitForPrompt() {
  await vi.waitFor(() => {
    expect(useInstanceStore.getState().elybySkinsPrompt).not.toBeNull();
  });
}

describe('instance list loading lifecycle', () => {
  beforeEach(() => {
    resetStore();
    invokeMock.mockReset();
  });

  it('clears loading and stores the error when the list request fails', async () => {
    invokeMock.mockRejectedValue(new Error('backend unavailable'));
    await useInstanceStore.getState().loadInstances();
    const state = useInstanceStore.getState();
    expect(state.isLoading).toBe(false);
    expect(state.error).toBeTruthy();
    expect(state.instances).toEqual([]);
  });
});

describe('instance deletion selection', () => {
  beforeEach(() => {
    resetStore();
    invokeMock.mockReset();
  });

  it('keeps a different selection made while the deletion is in flight', async () => {
    let releaseDelete!: () => void;
    const gate = new Promise<void>((resolve) => {
      releaseDelete = resolve;
    });
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'cmd_delete_instance') {
        await gate;
        return null;
      }
      if (command === 'cmd_list_instances') {
        return [{ name: 'B' }, { name: 'A' }] as any;
      }
      throw new Error(`unexpected command: ${command}`);
    });

    useInstanceStore.setState({ selectedInstance: 'A' });
    const deleting = useInstanceStore.getState().deleteInstance('A');
    // The user selects B while A is still being deleted.
    useInstanceStore.getState().selectInstance('B');
    releaseDelete();
    await deleting;

    expect(useInstanceStore.getState().selectedInstance).toBe('B');
    expect(useInstanceStore.getState().instances.map((i) => i.name)).toEqual(['B', 'A']);
  });

  it('clears the selection only when the selected instance itself is deleted', async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'cmd_delete_instance') return null;
      if (command === 'cmd_list_instances') return [] as any;
      throw new Error(`unexpected command: ${command}`);
    });
    useInstanceStore.setState({ selectedInstance: 'A' });
    await useInstanceStore.getState().deleteInstance('A');
    expect(useInstanceStore.getState().selectedInstance).toBeNull();
  });
});

describe('Ely.by skin preparation fallback', () => {
  beforeEach(() => {
    resetStore();
    invokeMock.mockReset();
  });

  it('retries preparation and then launches normally', async () => {
    let preflights = 0;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'cmd_prepare_elyby_skins') {
        preflights += 1;
        return preflights === 1
          ? { status: 'failed', kind: 'network', detail: 'offline' }
          : { status: 'ready', version: '1.2.8' };
      }
      if (command === 'cmd_launch_game') return 'Launching...';
      throw new Error(`unexpected command: ${command}`);
    });

    const launch = useInstanceStore.getState().launchGame('Test');
    await waitForPrompt();
    useInstanceStore.getState().resolveElybySkins('retry');
    await expect(launch).resolves.toBe(true);

    expect(preflights).toBe(2);
    expect(invokeMock).toHaveBeenCalledWith(
      'cmd_launch_game',
      expect.objectContaining({ instanceName: 'Test', skipElybySkins: false }),
    );
    expect(useInstanceStore.getState().elybySkinsPrompt).toBeNull();
  });

  it('queues concurrent Ely.by prompts instead of hanging the first launch', async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'cmd_prepare_elyby_skins') {
        return { status: 'failed', kind: 'network', detail: 'offline' };
      }
      if (command === 'cmd_launch_game') return 'Launching...';
      throw new Error(`unexpected command: ${command}`);
    });

    const first = useInstanceStore.getState().launchGame('First');
    const second = useInstanceStore.getState().launchGame('Second');
    await waitForPrompt();
    expect(useInstanceStore.getState().isLaunching).toBe(true);
    useInstanceStore.getState().resolveElybySkins('bypass');
    await expect(first).resolves.toBe(true);

    await waitForPrompt();
    useInstanceStore.getState().resolveElybySkins('bypass');
    await expect(second).resolves.toBe(true);
    expect(useInstanceStore.getState().elybySkinsPrompt).toBeNull();
    expect(useInstanceStore.getState().isLaunching).toBe(false);
  });

  it('launches once without the agent and does not persist the choice', async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'cmd_prepare_elyby_skins') {
        return { status: 'failed', kind: 'checksum', detail: 'bad hash' };
      }
      if (command === 'cmd_launch_game') return 'Launching...';
      throw new Error(`unexpected command: ${command}`);
    });

    const launch = useInstanceStore.getState().launchGame('Test');
    await waitForPrompt();
    useInstanceStore.getState().resolveElybySkins('bypass');
    await expect(launch).resolves.toBe(true);

    expect(invokeMock).toHaveBeenCalledWith(
      'cmd_launch_game',
      expect.objectContaining({ instanceName: 'Test', skipElybySkins: true }),
    );
    expect(invokeMock).not.toHaveBeenCalledWith('cmd_save_config', expect.anything());
    expect(useInstanceStore.getState().elybySkinsPrompt).toBeNull();
  });

  it('reopens the dialog when the second preparation inside launch fails', async () => {
    let launches = 0;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'cmd_prepare_elyby_skins') {
        return { status: 'ready', version: '1.2.8' };
      }
      if (command === 'cmd_launch_game') {
        launches += 1;
        if (launches === 1) {
          throw { elyby_skins: { kind: 'checksum', detail: 'bad hash' } };
        }
        return 'Launching...';
      }
      throw new Error(`unexpected command: ${command}`);
    });

    const launch = useInstanceStore.getState().launchGame('Test');
    await waitForPrompt();
    useInstanceStore.getState().resolveElybySkins('bypass');
    await expect(launch).resolves.toBe(true);

    expect(launches).toBe(2);
    expect(invokeMock).toHaveBeenLastCalledWith(
      'cmd_launch_game',
      expect.objectContaining({ instanceName: 'Test', skipElybySkins: true }),
    );
  });

  it('retries the launch after a structured second-preparation failure', async () => {
    let launches = 0;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'cmd_prepare_elyby_skins') {
        return { status: 'ready', version: '1.2.8' };
      }
      if (command === 'cmd_launch_game') {
        launches += 1;
        if (launches === 1) {
          throw { elyby_skins: { kind: 'network', detail: 'connection reset' } };
        }
        return 'Launching...';
      }
      throw new Error(`unexpected command: ${command}`);
    });

    const launch = useInstanceStore.getState().launchGame('Test');
    await waitForPrompt();
    useInstanceStore.getState().resolveElybySkins('retry');
    await expect(launch).resolves.toBe(true);

    expect(launches).toBe(2);
    expect(invokeMock).toHaveBeenLastCalledWith(
      'cmd_launch_game',
      expect.objectContaining({ instanceName: 'Test', skipElybySkins: false }),
    );
  });

  it('cancels without starting the game and clears the launch state', async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'cmd_prepare_elyby_skins') {
        return { status: 'failed', kind: 'metadata', detail: 'bad metadata' };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    const launch = useInstanceStore.getState().launchGame('Test');
    await waitForPrompt();
    useInstanceStore.getState().resolveElybySkins('cancel');
    await expect(launch).resolves.toBe(false);

    expect(invokeMock).not.toHaveBeenCalledWith('cmd_launch_game', expect.anything());
    expect(useInstanceStore.getState().isLaunching).toBe(false);
    expect(useInstanceStore.getState().launchStatus).toBeNull();
  });
});
