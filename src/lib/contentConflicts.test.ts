import { describe, expect, it } from 'vitest';
import type { SelectableContent } from './contentSelection';
import {
  conflictAffectedKeys,
  buildConflictIndex,
  candidateDependencyStatus,
  conflictItemKey,
  conflictProjectKey,
  planConflictResolution,
  pruneConflicts,
  withVersion,
  applyVersionChanges,
  type AvailableVersion,
  type ContentConflict,
} from './contentConflicts';

function item(overrides: Partial<SelectableContent> = {}): SelectableContent {
  return {
    name: 'Sodium',
    downloadUrl: 'https://cdn.example/sodium.jar',
    filename: 'sodium.jar',
    source: 'modrinth',
    projectId: 'sodium',
    versionId: 'sodium-094',
    versionName: 'mc26.3-0.9.4-fabric',
    ...overrides,
  };
}

function version(id: string, label: string, overrides: Partial<AvailableVersion> = {}): AvailableVersion {
  return {
    id,
    name: label,
    version_number: label,
    files: [
      {
        url: `https://cdn.example/${id}.jar`,
        filename: `${id}.jar`,
        primary: true,
        hashes: { sha1: `sha-${id}` },
        size: 42,
      },
    ],
    ...overrides,
  };
}

const irisConflict: ContentConflict = {
  provider: 'modrinth',
  project_id: 'sodium',
  project_name: 'Sodium',
  reasons: [
    {
      source_provider: 'modrinth',
      source_project_id: 'iris',
      source_name: 'Iris Shaders',
      source_version_id: 'iris-1',
      requirement: 'mc26.3-0.9.2-fabric',
      requirement_version_id: 'sodium-092',
      kind: 'version_mismatch',
    },
  ],
};

describe('conflict index', () => {
  it('marks the target and the source project as conflict participants', () => {
    const index = buildConflictIndex([irisConflict]);
    expect(index[conflictProjectKey('modrinth', 'sodium')].targetReasons).toHaveLength(1);
    expect(index[conflictProjectKey('modrinth', 'iris')].sourceLinks).toHaveLength(1);
    expect(index[conflictProjectKey('modrinth', 'iris')].sourceLinks[0].conflict.project_name)
      .toBe('Sodium');
  });

  it('lists affected cards target-first for manual navigation', () => {
    expect(conflictAffectedKeys([irisConflict])).toEqual([
      conflictProjectKey('modrinth', 'sodium'),
      conflictProjectKey('modrinth', 'iris'),
    ]);
  });

  it('keeps several reasons and independent conflicts without merging projects', () => {
    const second: ContentConflict = {
      provider: 'curseforge',
      project_id: '123',
      project_name: 'Other',
      reasons: [
        {
          source_provider: 'curseforge',
          source_project_id: '456',
          source_name: 'Source',
          requirement: '1.0',
          requirement_version_id: 'cf:123:1',
          kind: 'version_mismatch',
        },
      ],
    };
    const index = buildConflictIndex([irisConflict, second]);
    expect(index[conflictProjectKey('modrinth', 'sodium')].targetReasons).toHaveLength(1);
    expect(index[conflictProjectKey('curseforge', '123')].targetReasons).toHaveLength(1);
    expect(index[conflictProjectKey('curseforge', '456')].sourceLinks).toHaveLength(1);
  });

  it('drops the conflict when its source Iris is removed, keeping unrelated ones', () => {
    const unrelated: ContentConflict = {
      provider: 'modrinth',
      project_id: 'lithium',
      project_name: 'Lithium',
      reasons: [
        {
          source_provider: 'modrinth',
          source_project_id: 'other',
          source_name: 'Other',
          requirement: '1.0',
          requirement_version_id: 'lithium-1',
          kind: 'version_mismatch',
        },
      ],
    };
    const next = pruneConflicts([irisConflict, unrelated], 'modrinth', 'iris');
    expect(next).toHaveLength(1);
    expect(next[0].project_id).toBe('lithium');
  });

  it('drops both sides when the target is removed', () => {
    expect(pruneConflicts([irisConflict], 'modrinth', 'sodium')).toEqual([]);
  });
});

describe('version candidate dependency status', () => {
  const requirement = irisConflict.reasons[0];

  it('marks the currently selected wrong version as a conflict, not compatible', () => {
    expect(candidateDependencyStatus('sodium-094', [requirement])).toEqual({
      kind: 'conflict',
      sources: ['Iris Shaders'],
    });
  });

  it('marks the exact required version with the real source name', () => {
    expect(candidateDependencyStatus('sodium-092', [requirement])).toEqual({
      kind: 'required',
      sources: ['Iris Shaders'],
    });
    const secondSource: typeof requirement = {
      ...requirement,
      source_project_id: 'other',
      source_name: 'Other Mod',
    };
    expect(candidateDependencyStatus('sodium-092', [requirement, secondSource])).toEqual({
      kind: 'required',
      sources: ['Iris Shaders', 'Other Mod'],
    });
  });

  it('keeps a conflict when two selected mods demand different versions', () => {
    const second: typeof requirement = {
      ...requirement,
      source_project_id: 'other',
      source_name: 'Other Mod',
      requirement: 'mc26.3-0.9.0-fabric',
      requirement_version_id: 'sodium-090',
    };
    expect(candidateDependencyStatus('sodium-092', [requirement, second])).toEqual({
      kind: 'conflict',
      sources: ['Other Mod'],
    });
    expect(candidateDependencyStatus('sodium-090', [requirement, second])).toEqual({
      kind: 'conflict',
      sources: ['Iris Shaders'],
    });
  });

  it('does not claim compatibility when no exact requirement is known', () => {
    expect(candidateDependencyStatus('sodium-092', [])).toEqual({ kind: 'unknown' });
    expect(
      candidateDependencyStatus('sodium-092', [
        { ...requirement, requirement_version_id: null },
      ]),
    ).toEqual({ kind: 'unknown' });
  });

  it('stays unknown when an exact match coexists with an unverified requirement', () => {
    const unverified: typeof requirement = {
      ...requirement,
      source_project_id: 'other',
      source_name: 'Other Mod',
      requirement: 'some-range',
      requirement_version_id: null,
    };
    expect(candidateDependencyStatus('sodium-092', [requirement, unverified]))
      .toEqual({ kind: 'unknown' });
  });

  it('marks every candidate as conflicting with an incompatible selected mod', () => {
    const incompatible: typeof requirement = {
      ...requirement,
      kind: 'incompatible',
      requirement: '',
      requirement_version_id: null,
    };
    expect(candidateDependencyStatus('sodium-092', [incompatible])).toEqual({
      kind: 'conflict',
      sources: ['Iris Shaders'],
    });
  });
});

describe('automatic conflict plan', () => {
  const sodium = item();
  const versions = {
    [conflictProjectKey('modrinth', 'sodium')]: [
      version('sodium-094', 'mc26.3-0.9.4-fabric'),
      version('sodium-092', 'mc26.3-0.9.2-fabric'),
    ],
  };

  it('proposes the exact required version without applying it', () => {
    const plan = planConflictResolution([irisConflict], [sodium], versions);
    expect(plan.unresolved).toHaveLength(0);
    expect(plan.changes).toHaveLength(1);
    expect(plan.changes[0].version.id).toBe('sodium-092');
    expect(plan.changes[0].fromLabel).toBe('mc26.3-0.9.4-fabric');
    expect(plan.changes[0].toLabel).toBe('mc26.3-0.9.2-fabric');
    expect(plan.changes[0].reasons[0].source_name).toBe('Iris Shaders');
    expect(sodium.versionId).toBe('sodium-094');
  });

  it('uses a unique legacy label when the resolver did not provide an id', () => {
    const legacy: ContentConflict = {
      ...irisConflict,
      reasons: [{ ...irisConflict.reasons[0], requirement_version_id: null }],
    };
    const plan = planConflictResolution([legacy], [sodium], versions);
    expect(plan.changes).toHaveLength(1);
    expect(plan.changes[0].version.id).toBe('sodium-092');
  });

  it('does not guess when a legacy label matches several versions', () => {
    const duplicateVersions = {
      [conflictProjectKey('modrinth', 'sodium')]: [
        version('a', 'same-label'),
        version('b', 'same-label'),
      ],
    };
    const conflict: ContentConflict = {
      ...irisConflict,
      reasons: [{ ...irisConflict.reasons[0], requirement: 'same-label', requirement_version_id: null }],
    };
    const plan = planConflictResolution([conflict], [sodium], duplicateVersions);
    expect(plan.changes).toHaveLength(0);
    expect(plan.unresolved[0].code).toBe('ambiguous');
  });

  it('reports a missing candidate instead of choosing a different version', () => {
    const plan = planConflictResolution([irisConflict], [sodium], {
      [conflictProjectKey('modrinth', 'sodium')]: [version('sodium-094', 'mc26.3-0.9.4-fabric')],
    });
    expect(plan.changes).toHaveLength(0);
    expect(plan.unresolved[0].code).toBe('no_candidate');
  });

  it('does not auto-change incompatible conflicts', () => {
    const conflict: ContentConflict = {
      ...irisConflict,
      reasons: [{ ...irisConflict.reasons[0], kind: 'incompatible', requirement: '' }],
    };
    const plan = planConflictResolution([conflict], [sodium], versions);
    expect(plan.changes).toHaveLength(0);
    expect(plan.unresolved[0].code).toBe('incompatible');
  });

  it('does not edit resolver-managed dependencies', () => {
    const dependency = item({ isDependency: true });
    const plan = planConflictResolution([irisConflict], [dependency], versions);
    expect(plan.changes).toHaveLength(0);
    expect(plan.unresolved[0].code).toBe('dependency_managed');
  });

  it('requires provider data before proposing a change', () => {
    const plan = planConflictResolution([irisConflict], [sodium], {});
    expect(plan.changes).toHaveLength(0);
    expect(plan.unresolved[0].code).toBe('missing_versions');
  });

  it('refuses an automatic change when two requirements demand different versions', () => {
    const secondRequirement: ContentConflict = {
      ...irisConflict,
      reasons: [
        {
          ...irisConflict.reasons[0],
          source_project_id: 'other',
          source_name: 'Other Mod',
          source_version_id: 'other-1',
          requirement: 'mc26.3-0.9.0-fabric',
          requirement_version_id: 'sodium-090',
        },
      ],
    };
    const plan = planConflictResolution(
      [irisConflict, secondRequirement],
      [sodium],
      {
        [conflictProjectKey('modrinth', 'sodium')]: [
          version('sodium-094', 'mc26.3-0.9.4-fabric'),
          version('sodium-092', 'mc26.3-0.9.2-fabric'),
          version('sodium-090', 'mc26.3-0.9.0-fabric'),
        ],
      },
    );
    expect(plan.changes).toHaveLength(0);
    expect(plan.unresolved.some((entry) => entry.code === 'ambiguous')).toBe(true);
  });

  it('builds one independent change per conflict', () => {
    const lithium = item({
      name: 'Lithium',
      projectId: 'lithium',
      versionId: 'lithium-old',
      versionName: 'old',
    });
    const lithiumConflict: ContentConflict = {
      provider: 'modrinth',
      project_id: 'lithium',
      project_name: 'Lithium',
      reasons: [
        {
          source_provider: 'modrinth',
          source_project_id: 'other',
          source_name: 'Other',
          requirement: 'new',
          requirement_version_id: 'lithium-new',
          kind: 'version_mismatch',
        },
      ],
    };
    const plan = planConflictResolution(
      [irisConflict, lithiumConflict],
      [sodium, lithium],
      {
        ...versions,
        [conflictProjectKey('modrinth', 'lithium')]: [
          version('lithium-old', 'old'),
          version('lithium-new', 'new'),
        ],
      },
    );
    expect(plan.changes).toHaveLength(2);
    expect(plan.changes.map((change) => change.version.id).sort()).toEqual([
      'lithium-new',
      'sodium-092',
    ]);
  });
});

describe('manual version choice', () => {
  it('replaces the existing project entry, keeping one version per project', () => {
    const sodium = item();
    const lithium = item({ name: 'Lithium', projectId: 'lithium', versionId: 'l1' });
    const chosen = version('sodium-092', 'mc26.3-0.9.2-fabric');
    const next = applyVersionChanges([sodium, lithium], [
      { key: conflictItemKey(sodium), version: chosen },
    ]);

    expect(next).toHaveLength(2);
    expect(next[0].versionId).toBe('sodium-092');
    expect(next[0].filename).toBe('sodium-092.jar');
    expect(next[0].sha1).toBe('sha-sodium-092');
    expect(next[1]).toEqual(lithium);
  });

  it('does not mutate the original selection', () => {
    const sodium = item();
    const original = { ...sodium };
    applyVersionChanges([sodium], [
      { key: conflictItemKey(sodium), version: version('sodium-092', 'mc26.3-0.9.2-fabric') },
    ]);
    expect(sodium).toEqual(original);
  });

  it('returns null when a version has no downloadable file', () => {
    expect(withVersion(item(), { id: 'x', name: 'x', version_number: 'x', files: [] })).toBeNull();
  });
});
