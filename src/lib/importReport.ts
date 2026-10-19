/** Mirrors the Rust `FailedImportFile` returned by the import commands. */
export interface FailedImportFile {
  file_name: string;
  project_id: number | null;
  file_id: number | null;
  reason: string;
  /** False for broken manifest entries (no ids to retry with). */
  retryable: boolean;
}

/** Mirrors the Rust `ImportReport`. */
export interface ImportReport {
  total_files: number;
  installed: number;
  failed: FailedImportFile[];
  skipped: number;
  warnings: string[];
}

/** Mirrors the Rust `ImportOutcome` (only the fields the UI reads). */
export interface ImportOutcome {
  instance: { name: string };
  report: ImportReport;
}

/** True when the pack is usable but not fully installed. */
export function reportHasGaps(report: ImportReport): boolean {
  return report.installed < report.total_files || report.failed.length > 0;
}
