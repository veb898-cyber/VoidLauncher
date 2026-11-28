export type LocalCompatibilityStatus = 'confirmed' | 'potential' | 'unknown';

export interface LocalCompatibilityFinding {
  code: string;
  status: LocalCompatibilityStatus;
  source: string;
  basis: string;
  explanation: string;
  affectedFiles: string[];
  affectedModIds: string[];
  dependency?: {
    modId: string;
    kind: string;
    requirement?: string | null;
    alternatives: string[];
    providers: Array<{ filename: string; version?: string | null }>;
  };
}

export interface LocalCompatibilityReport {
  complete: boolean;
  filesAnalyzed: number;
  findings: LocalCompatibilityFinding[];
}

export type CompatibilityFilter = 'all' | LocalCompatibilityStatus;

export class CompatibilityRequestTracker {
  private generation = 0;
  private pendingGeneration: number | null = null;
  private rerunRequested = false;

  invalidate(): void {
    this.generation += 1;
    if (this.pendingGeneration !== null) this.rerunRequested = true;
  }

  begin(): number | null {
    if (this.pendingGeneration !== null) return null;
    this.generation += 1;
    this.pendingGeneration = this.generation;
    return this.generation;
  }

  isCurrent(generation: number): boolean {
    return generation === this.generation;
  }

  finish(generation: number): boolean {
    if (this.pendingGeneration !== generation) return false;
    this.pendingGeneration = null;
    const rerunRequested = this.rerunRequested;
    this.rerunRequested = false;
    return rerunRequested;
  }
}

/** Coalesce watcher events that arrive while ContentManager intentionally defers reloads. */
export class DeferredCompatibilityRefresh {
  private pending = false;

  defer(): void {
    this.pending = true;
  }

  consume(isBlocked: boolean): boolean {
    if (isBlocked) return false;
    const pending = this.pending;
    this.pending = false;
    return pending;
  }

  clear(): void {
    this.pending = false;
  }
}

export function countCompatibilityFindings(findings: LocalCompatibilityFinding[]) {
  return findings.reduce(
    (counts, finding) => {
      counts.total += 1;
      counts[finding.status] += 1;
      return counts;
    },
    { total: 0, confirmed: 0, potential: 0, unknown: 0 },
  );
}

export function filterCompatibilityFindings(
  findings: LocalCompatibilityFinding[],
  filter: CompatibilityFilter,
): LocalCompatibilityFinding[] {
  return filter === 'all' ? findings : findings.filter((finding) => finding.status === filter);
}
