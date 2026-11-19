/**
 * Shared progress math for file and multi-file downloads.
 *
 * Rules (mirrors the backend progress tracker):
 * - an unknown/zero total means indeterminate — never invent 100%;
 * - in-progress operations are capped at 99%;
 * - only a confirmed completion may return 100%;
 * - byte totals, when known, are weighted by size instead of averaging
 *   per-file percentages or treating every file as equally large.
 */

export interface FileProgressPayload {
  url: string;
  /** Destination file name, so concurrent downloads can be told apart. */
  file_name: string;
  downloaded: number;
  /** Null when the server did not provide a trustworthy Content-Length. */
  total: number | null;
  /** Opaque id of the content-install request that owns this download. */
  request_id?: string | null;
}

function clamp(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, value));
}

/** Percent for a single file; null = indeterminate. */
export function fileProgressPercent(
  downloaded: number,
  total: number | null | undefined,
  done = false,
): number | null {
  if (!total || total <= 0) return null;
  const percent = clamp((downloaded / total) * 100, 0, 100);
  return done ? percent : Math.min(99, percent);
}

/**
 * Weighted percent for a group of files. `completedBytes` must contain only
 * verified/completed files; `currentBytes` is the newest in-flight chunk of
 * the current file. Retries should reset `currentBytes`, never re-add the
 * previous attempt.
 */
export function weightedProgressPercent(
  completedBytes: number,
  currentBytes: number,
  totalBytes: number,
  done = false,
): number | null {
  if (!totalBytes || totalBytes <= 0) return null;
  const percent = clamp(((completedBytes + currentBytes) / totalBytes) * 100, 0, 100);
  return done ? percent : Math.min(99, percent);
}

/** Count-based fallback when file sizes are unavailable. */
export function countProgressPercent(
  completed: number,
  total: number,
  done = false,
): number | null {
  if (!total || total <= 0) return null;
  const percent = clamp((completed / total) * 100, 0, 100);
  return done ? percent : Math.min(99, percent);
}

export interface ImportProgressLike {
  stage: string;
  current: number;
  total: number;
  bytes_done?: number;
  bytes_total?: number;
}

const INDETERMINATE_STAGES = ['reading', 'extracting', 'loader', 'manifest'];

/**
 * Overall import/modpack percent. `currentFileBytes` is added only for the
 * byte-aware `files`/`assets` stages; the backend's `bytes_done` counts whole
 * completed files and the in-flight global file event covers the rest.
 */
export function importProgressPercent(
  progress: ImportProgressLike,
  currentFileBytes = 0,
): number | null {
  if (progress.stage === 'done') return 100;
  if (INDETERMINATE_STAGES.includes(progress.stage)) return null;
  if (progress.bytes_total && progress.bytes_total > 0) {
    return weightedProgressPercent(
      progress.bytes_done ?? 0,
      progress.stage === 'files' || progress.stage === 'assets'
        ? currentFileBytes
        : 0,
      progress.bytes_total,
    );
  }
  return countProgressPercent(progress.current, progress.total);
}
