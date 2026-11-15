/**
 * Conflict presentation and safe auto-resolution planning for the content
 * install cart.
 *
 * The backend resolver is the source of truth: it knows which exact version a
 * dependency edge named. The UI must never "fix" a conflict by comparing
 * version strings or assuming that a newer version is compatible. A proposed
 * replacement is therefore built only from an explicit `version_id` supplied
 * by the resolver (or, for legacy payloads, a unique exact label match).
 */
import type { SelectableContent } from './contentSelection';

export interface ConflictReason {
  source_provider?: string;
  source_project_id: string;
  source_name: string;
  source_version_id?: string | null;
  requirement: string;
  requirement_version_id?: string | null;
  kind: string;
}

export interface ContentConflict {
  provider?: string;
  project_id: string;
  project_name: string;
  reasons: ConflictReason[];
}

export interface ConflictSourceLink {
  conflict: ContentConflict;
  reason: ConflictReason;
}

/** What one selected project looks like from both sides of conflict edges. */
export interface ConflictEntry {
  targetReasons: ConflictReason[];
  sourceLinks: ConflictSourceLink[];
}

export type ConflictIndex = Record<string, ConflictEntry>;

export function conflictProjectKey(provider: string | undefined, projectId: string): string {
  return `${provider || 'modrinth'}:${projectId}`;
}

export function conflictItemKey(
  item: Pick<SelectableContent, 'source' | 'projectId'>,
): string {
  return conflictProjectKey(item.source, item.projectId);
}

export function buildConflictIndex(conflicts: readonly ContentConflict[]): ConflictIndex {
  const index: ConflictIndex = {};
  for (const conflict of conflicts) {
    const targetKey = conflictProjectKey(conflict.provider, conflict.project_id);
    const entry = index[targetKey] ?? { targetReasons: [], sourceLinks: [] };
    entry.targetReasons.push(...conflict.reasons);
    index[targetKey] = entry;

    for (const reason of conflict.reasons) {
      if (!reason.source_project_id) continue;
      const sourceKey = conflictProjectKey(reason.source_provider, reason.source_project_id);
      const sourceEntry = index[sourceKey] ?? { targetReasons: [], sourceLinks: [] };
      sourceEntry.sourceLinks.push({ conflict, reason });
      index[sourceKey] = sourceEntry;
    }
  }
  return index;
}

/**
 * Deterministic list of cards involved in conflicts: each target first, then
 * the projects that declare the requirement. Used by "show next conflict";
 * it never scrolls on its own.
 */
export function conflictAffectedKeys(conflicts: readonly ContentConflict[]): string[] {  const keys: string[] = [];
  const seen = new Set<string>();
  const push = (key: string) => {
    if (seen.has(key)) return;
    seen.add(key);
    keys.push(key);
  };
  for (const conflict of conflicts) {
    push(conflictProjectKey(conflict.provider, conflict.project_id));
  }
  for (const conflict of conflicts) {
    for (const reason of conflict.reasons) {
      if (reason.source_project_id) {
        push(conflictProjectKey(reason.source_provider, reason.source_project_id));
      }
    }
  }
  return keys;
}

/**
 * Drop the conflict entries caused by one project. Removing the target drops
 * its conflict; removing the source drops every reason it declared and then
 * the now-empty conflict. Unrelated conflicts are preserved.
 */
export function pruneConflicts(
  conflicts: readonly ContentConflict[],
  provider: string | undefined,
  projectId: string,
): ContentConflict[] {
  const key = conflictProjectKey(provider, projectId);
  return conflicts
    .filter((conflict) => conflictProjectKey(conflict.provider, conflict.project_id) !== key)
    .map((conflict) => ({
      ...conflict,
      reasons: conflict.reasons.filter(
        (reason) =>
          conflictProjectKey(reason.source_provider, reason.source_project_id) !== key,
      ),
    }))
    .filter((conflict) => conflict.reasons.length > 0);
}

/** Available catalog version (structurally compatible with ProviderVersion). */
export interface AvailableVersion {
  id: string;
  name: string;
  version_number: string;
  version_type?: string;
  game_versions?: string[];
  loaders?: string[];
  files?: Array<{
    url: string;
    filename: string;
    primary?: boolean;
    hashes?: Record<string, string>;
    size?: number;
    file_id?: string;
  }>;
}

export type UnresolvedConflictCode =
  | 'incompatible'
  | 'ambiguous'
  | 'no_candidate'
  | 'dependency_managed'
  | 'missing_versions'
  | 'not_selected';

/** Dependency-based status of one version candidate in the version editor. */
export type CandidateDependencyStatus =
  | { kind: 'required'; sources: string[] }
  | { kind: 'conflict'; sources: string[] }
  | { kind: 'unknown' };

function uniqueNames(names: readonly string[]): string[] {
  return [...new Set(names.filter(Boolean))];
}

/**
 * Evaluate a version candidate only against the exact requirements the
 * resolver already reported for the selected project.
 *
 * - no exact requirement at all → `unknown` (the catalog already filtered the
 *   Minecraft/loader side; absence of a conflict is not proof of compatibility);
 * - candidate is the exact required version and every known requirement agrees
 *   → `required` with the real source names;
 * - candidate violates any exact requirement → `conflict`;
 * - any requirement without a version id → `unknown` unless the candidate
 *   already violates a different exact requirement.
 */
export function candidateDependencyStatus(
  versionId: string,
  reasons: readonly ConflictReason[],
): CandidateDependencyStatus {
  if (reasons.length === 0) return { kind: 'unknown' };

  const incompatible = reasons.filter((reason) => reason.kind !== 'version_mismatch');
  if (incompatible.length > 0) {
    return { kind: 'conflict', sources: uniqueNames(incompatible.map((r) => r.source_name)) };
  }

  const exact = reasons.filter(
    (reason): reason is ConflictReason & { requirement_version_id: string } =>
      !!reason.requirement_version_id,
  );
  const unverified = reasons.filter((reason) => !reason.requirement_version_id);
  if (exact.length === 0) return { kind: 'unknown' };

  const requiredIds = new Set(exact.map((reason) => reason.requirement_version_id));
  const matching = exact.filter((reason) => reason.requirement_version_id === versionId);
  const unmet = exact.filter((reason) => reason.requirement_version_id !== versionId);

  if (matching.length === 0) {
    return { kind: 'conflict', sources: uniqueNames(exact.map((reason) => reason.source_name)) };
  }
  if (requiredIds.size > 1) {
    // The candidate satisfies one source but another one demands a different
    // exact version: the incompatibility is real and must stay visible.
    return {
      kind: 'conflict',
      sources: uniqueNames(unmet.map((reason) => reason.source_name)),
    };
  }
  if (unverified.length > 0) return { kind: 'unknown' };
  return {
    kind: 'required',
    sources: uniqueNames(matching.map((reason) => reason.source_name)),
  };
}

export interface UnresolvedConflict {
  conflict: ContentConflict;
  code: UnresolvedConflictCode;
}

export interface PlannedVersionChange<T extends SelectableContent> {
  key: string;
  item: T;
  version: AvailableVersion;
  fromLabel: string;
  toLabel: string;
  reasons: ConflictReason[];
  targetName: string;
}

export interface ConflictAutoPlan<T extends SelectableContent> {
  changes: PlannedVersionChange<T>[];
  unresolved: UnresolvedConflict[];
}

function uniqueVersionIdForReason(
  reason: ConflictReason,
  versions: readonly AvailableVersion[],
): string | null {
  if (reason.requirement_version_id) return reason.requirement_version_id;
  if (!reason.requirement) return null;
  // Legacy resolver payloads only carry a human label. Accept it only when
  // exactly one available version matches; never pick "the newest" as a guess.
  const matches = versions.filter(
    (version) =>
      version.version_number === reason.requirement || version.name === reason.requirement,
  );
  return matches.length === 1 ? matches[0].id : null;
}

function labelOf(version: AvailableVersion): string {
  return version.name || version.version_number || version.id;
}

/**
 * Build a confirmation plan for version_mismatch conflicts.
 *
 * A change is planned only when the target is a user-selected root, every
 * reason names the same exact version, and that version is present in the
 * provider list. Anything else is reported as unresolved instead of guessed.
 */
export function planConflictResolution<T extends SelectableContent>(
  conflicts: readonly ContentConflict[],
  selected: readonly T[],
  versionsByProject: Readonly<Record<string, readonly AvailableVersion[] | undefined>>,
): ConflictAutoPlan<T> {
  const selectedByKey = new Map<string, T>();
  for (const item of selected) selectedByKey.set(conflictItemKey(item), item);

  const proposals = new Map<
    string,
    {
      item: T;
      versionId: string;
      reasons: ConflictReason[];
      targetName: string;
    }
  >();
  const ambiguousKeys = new Set<string>();
  const unresolved: UnresolvedConflict[] = [];

  for (const conflict of conflicts) {
    const key = conflictProjectKey(conflict.provider, conflict.project_id);
    if (ambiguousKeys.has(key)) {
      unresolved.push({ conflict, code: 'ambiguous' });
      continue;
    }
    if (conflict.reasons.some((reason) => reason.kind !== 'version_mismatch')) {
      unresolved.push({ conflict, code: 'incompatible' });
      continue;
    }
    const item = selectedByKey.get(key);
    if (!item) {
      unresolved.push({ conflict, code: 'not_selected' });
      continue;
    }
    if (item.isDependency) {
      // Dependencies are rebuilt by the resolver on every pass; a manual
      // edit here would be discarded. Only user-owned roots are changed.
      unresolved.push({ conflict, code: 'dependency_managed' });
      continue;
    }
    const versions = versionsByProject[key];
    if (!versions || versions.length === 0) {
      unresolved.push({ conflict, code: 'missing_versions' });
      continue;
    }

    const candidates = new Set<string>();
    let failed = false;
    for (const reason of conflict.reasons) {
      const requiredId = uniqueVersionIdForReason(reason, versions);
      if (!requiredId) {
        unresolved.push({
          conflict,
          code: reason.requirement ? 'ambiguous' : 'no_candidate',
        });
        failed = true;
        break;
      }
      if (!versions.some((version) => version.id === requiredId)) {
        unresolved.push({ conflict, code: 'no_candidate' });
        failed = true;
        break;
      }
      const candidate = versions.find((version) => version.id === requiredId);
      if (!candidate || !candidate.files?.length) {
        unresolved.push({ conflict, code: 'no_candidate' });
        failed = true;
        break;
      }
      candidates.add(requiredId);
    }
    if (failed) continue;
    if (candidates.size !== 1) {
      unresolved.push({ conflict, code: 'ambiguous' });
      continue;
    }

    const versionId = [...candidates][0];
    const existing = proposals.get(key);
    if (existing && existing.versionId !== versionId) {
      // Two independent requirements want different versions of the same
      // project: there is no single safe automatic change.
      proposals.delete(key);
      ambiguousKeys.add(key);
      unresolved.push({ conflict, code: 'ambiguous' });
      continue;
    }
    if (existing) {
      existing.reasons.push(...conflict.reasons);
    } else {
      proposals.set(key, {
        item,
        versionId,
        reasons: [...conflict.reasons],
        targetName: conflict.project_name,
      });
    }
  }

  const changes: PlannedVersionChange<T>[] = [];
  for (const [key, proposal] of proposals) {
    const versions = versionsByProject[key] ?? [];
    const version = versions.find((candidate) => candidate.id === proposal.versionId);
    if (!version) {
      const conflict = conflicts.find(
        (candidate) => conflictProjectKey(candidate.provider, candidate.project_id) === key,
      );
      if (conflict) unresolved.push({ conflict, code: 'no_candidate' });
      continue;
    }
    if (proposal.item.versionId === version.id) continue;
    changes.push({
      key,
      item: proposal.item,
      version,
      fromLabel: proposal.item.versionName || proposal.item.versionId || proposal.item.filename,
      toLabel: labelOf(version),
      reasons: proposal.reasons,
      targetName: proposal.targetName,
    });
  }

  return { changes, unresolved };
}

/** Replace one selected item's version without adding a duplicate project. */
export function withVersion<T extends SelectableContent>(
  item: T,
  version: AvailableVersion,
): T | null {
  const file = version.files?.find((candidate) => candidate.primary) ?? version.files?.[0];
  if (!file) return null;
  return {
    ...item,
    downloadUrl: file.url,
    filename: file.filename,
    versionId: version.id,
    versionName: version.name || version.version_number,
    sha1: file.hashes?.sha1,
    fileId: file.file_id,
    size: file.size,
  };
}

/** Apply a confirmed plan atomically; untouched items keep identity. */
export function applyVersionChanges<T extends SelectableContent>(
  selected: readonly T[],
  changes: ReadonlyArray<{ key: string; version: AvailableVersion }>,
): T[] {
  const byKey = new Map(changes.map((change) => [change.key, change.version]));
  return selected.map((item) => {
    const version = byKey.get(conflictItemKey(item));
    if (!version) return item;
    return withVersion(item, version) ?? item;
  });
}
