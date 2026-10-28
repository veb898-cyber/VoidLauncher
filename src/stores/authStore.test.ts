import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useAuthStore } from './authStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const invokeMock = vi.mocked(invoke);

function resetStore() {
  useAuthStore.setState({
    profile: null,
    isLoggedIn: false,
    isLoading: false,
    error: null,
    deviceCode: null,
    userCode: null,
    verificationUri: null,
  });
}

describe('authStore', () => {
  beforeEach(() => {
    resetStore();
    invokeMock.mockReset();
  });

  it('checkAuth clears a stale profile after the account was removed', async () => {
    useAuthStore.setState({
      profile: { id: 'uuid-1', name: 'Steve' },
      isLoggedIn: true,
    });
    invokeMock.mockResolvedValue({ profile: null, offline_mode: false });

    await useAuthStore.getState().checkAuth();

    expect(useAuthStore.getState().profile).toBeNull();
    expect(useAuthStore.getState().isLoggedIn).toBe(false);
  });

  it('startLogin clears every trace of the previous device code', async () => {
    useAuthStore.setState({
      deviceCode: 'old-device-code',
      userCode: 'OLDCODE',
      verificationUri: 'https://www.microsoft.com/link',
    });
    invokeMock.mockRejectedValue('network down');

    await useAuthStore.getState().startLogin();

    const state = useAuthStore.getState();
    expect(state.deviceCode).toBeNull();
    expect(state.userCode).toBeNull();
    expect(state.verificationUri).toBeNull();
    expect(state.isLoading).toBe(false);
    expect(state.error).toContain('network down');
  });

  it('startLogin stores the Microsoft device-code response', async () => {
    invokeMock.mockResolvedValue({
      device_code: 'device-1',
      user_code: 'ABCD1234',
      verification_uri: 'https://www.microsoft.com/link',
      expires_in: 900,
      interval: 5,
    });

    await useAuthStore.getState().startLogin();

    const state = useAuthStore.getState();
    expect(state.deviceCode).toBe('device-1');
    expect(state.userCode).toBe('ABCD1234');
    expect(state.verificationUri).toBe('https://www.microsoft.com/link');
    expect(state.isLoading).toBe(false);
    expect(state.error).toBeNull();
  });
});
