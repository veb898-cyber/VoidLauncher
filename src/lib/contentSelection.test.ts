import { describe, expect, it } from 'vitest';
import {
  mergeResolvedSelection,
  pinSameProjectVersion,
  removeProject,
  type SelectableContent,
} from './contentSelection';

function item(overrides: Partial<SelectableContent> = {}): SelectableContent {
  return {
    name: 'Sodium',
    downloadUrl: 'https://cdn.example/sodium.jar',
    filename: 'sodium.jar',
    source: 'modrinth',
    projectId: 'sodium',
    versionId: 'v1',
    versionName: '1.0',
    ...overrides,
  };
}

describe('content selection pinning', () => {
  it('replaces the version of the same project instead of adding a duplicate', () => {
    const current = [item({ versionId: 'old', filename: 'old.jar' })];
    const next = pinSameProjectVersion(current, item({ versionId: 'new', filename: 'new.jar' }));

    expect(next).toHaveLength(1);
    expect(next[0].versionId).toBe('new');
    expect(next[0].filename).toBe('new.jar');
  });

  it('keeps other selected files untouched', () => {
    const lithium = item({ name: 'Lithium', projectId: 'lithium', versionId: 'l1' });
    const next = pinSameProjectVersion(
      [item({ versionId: 'old' }), lithium],
      item({ versionId: 'new' }),
    );

    expect(next.map((i) => i.projectId)).toEqual(['sodium', 'lithium']);
    expect(next[1].versionId).toBe('l1');
  });

  it('does not confuse the same project id across providers', () => {
    const curseforge = item({ source: 'curseforge', versionId: 'cf-1' });
    const next = pinSameProjectVersion([curseforge], item({ source: 'modrinth', versionId: 'mr-2' }));

    expect(next).toHaveLength(2);
    expect(next[0].versionId).toBe('cf-1');
    expect(next[1].versionId).toBe('mr-2');
  });

  it('keeps the explicitly selected root version when a late resolution returns another version', () => {
    const current = [item({ versionId: 'pinned', filename: 'sodium-0.6.jar' })];
    const resolved = [
      item({ versionId: 'newer', filename: 'sodium-0.7.jar', isDependency: false }),
    ];

    const merged = mergeResolvedSelection(current, resolved);

    expect(merged).toHaveLength(1);
    expect(merged[0].versionId).toBe('pinned');
    expect(merged[0].filename).toBe('sodium-0.6.jar');
  });

  it('takes dependencies from the resolver even when the same project was selected before', () => {
    const current = [item({ versionId: 'pinned' })];
    const resolved = [
      item({
        name: 'Dependency',
        projectId: 'dep',
        versionId: 'dep-version',
        isDependency: true,
        parentMod: 'Sodium',
      }),
      item({ versionId: 'newer', isDependency: false }),
    ];

    const merged = mergeResolvedSelection(current, resolved);

    expect(merged[0].projectId).toBe('dep');
    expect(merged[0].versionId).toBe('dep-version');
    expect(merged[1].versionId).toBe('pinned');
  });

  it('removes only the requested provider project, keeping the rest', () => {
    const sodium = item({ versionId: 's' });
    const iris = item({ projectId: 'iris', versionId: 'i' });
    const cf = item({ source: 'curseforge', projectId: 's', versionId: 'cf' });
    const next = removeProject([sodium, iris, cf], 'modrinth', 'sodium');

    expect(next.map((entry) => entry.projectId)).toEqual(['iris', 's']);
    expect(next[1].source).toBe('curseforge');
  });
});
