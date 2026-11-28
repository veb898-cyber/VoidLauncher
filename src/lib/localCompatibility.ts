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

  invalidate(): void {
    this.generation += 1;
    this.pendingGeneration = null;
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

  finish(generation: number): void {
    if (this.pendingGeneration === generation) this.pendingGeneration = null;
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
