import { beforeEach, describe, expect, it, vi } from 'vitest';

const { invoke, addToast } = vi.hoisted(() => ({
  invoke: vi.fn(),
  addToast: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../components/ui/Toast', () => ({ addToast }));
vi.mock('./instanceStore', () => ({
  useInstanceStore: {
    getState: () => ({ instances: [], loadInstances: async () => {} }),
  },
}));

import { useModpacksStore, type MrHit, type MrVersion } from './modpacksStore';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function hit(projectId: string, title: string): MrHit {
  return {
    project_id: projectId,
    title,
    description: '',
    icon_url: null,
    downloads: 0,
    slug: projectId,
  };
}

function version(id: string): MrVersion {
  return {
    id,
    project_id: id,
    name: id,
    version_number: id,
    game_versions: [],
    files: [],
  };
}

describe('modpacks stale response guards', () => {
  beforeEach(() => {
    invoke.mockReset();
    addToast.mockReset();
    useModpacksStore.setState({
      tab: 'modrinth',
      query: '',
      mrResults: [],
      mrOffset: 0,
      mrHasMore: true,
      loadingByTab: { modrinth: false, curseforge: false, atlauncher: false },
      loadingMore: false,
      selected: null,
      versions: [],
      loadingDetail: false,
    });
  });

  it('keeps the latest selected pack when an older detail request resolves late', async () => {
    const slow = deferred<MrVersion[]>();
    const fresh = deferred<MrVersion[]>();
    invoke.mockReturnValueOnce(slow.promise).mockReturnValueOnce(fresh.promise);

    const first = useModpacksStore.getState().selectMr(hit('old', 'Old pack'));
    const second = useModpacksStore.getState().selectMr(hit('new', 'New pack'));

    fresh.resolve([version('new')]);
    await second;
    slow.resolve([version('old')]);
    await first;

    const state = useModpacksStore.getState();
    expect(state.selected && 'project_id' in state.selected ? state.selected.project_id : null).toBe('new');
    expect(state.versions.map((v) => v.id)).toEqual(['new']);
    expect(state.loadingDetail).toBe(false);
  });

  it('keeps the latest search results when an older search resolves late', async () => {
    const slow = deferred<{ hits: MrHit[] }>();
    const fresh = deferred<{ hits: MrHit[] }>();
    invoke.mockReturnValueOnce(slow.promise).mockReturnValueOnce(fresh.promise);

    useModpacksStore.setState({ query: 'old query' });
    const first = useModpacksStore.getState().search();
    useModpacksStore.setState({ query: 'new query' });
    const second = useModpacksStore.getState().search();

    fresh.resolve({ hits: [hit('new', 'New result')] });
    await second;
    slow.resolve({ hits: [hit('old', 'Old result')] });
    await first;

    const state = useModpacksStore.getState();
    expect(state.mrResults.map((h) => h.project_id)).toEqual(['new']);
    expect(state.loadingByTab.modrinth).toBe(false);
  });
});
