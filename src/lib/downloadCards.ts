/**
 * Per-card download state for the content install queue.
 *
 * The group progress bar belongs to the whole operation; each card owns the
 * bytes of exactly one file. Events carry absolute byte counts for the current
 * attempt, so a retry replaces the previous value instead of adding to it.
 */
import {
  countProgressPercent,
  fileProgressPercent,
  weightedProgressPercent,
} from './downloadProgress';

export type CardDownloadStatus = 'queued' | 'downloading' | 'done' | 'error';

export interface DownloadCardState {
  status: CardDownloadStatus;
  downloaded: number;
  /** Null when the server did not report a trustworthy size. */
  total: number | null;
}

export type DownloadCardMap = Record<string, DownloadCardState>;

export interface DownloadCardIdentity {
  source?: string;
  projectId: string;
  versionId?: string;
  filename: string;
  /** Provider-reported size, when available. */
  size?: number;
}

/**
 * Stable key for one queued file. The provider + project + pinned version
 * identify the selection; the filename is only a fallback for local/legacy
 * entries without ids, so two files with the same name cannot collide.
 */
export function downloadCardKey(item: DownloadCardIdentity): string {
  const provider = item.source || 'modrinth';
  const version = item.versionId || `file:${item.filename}`;
  return `${provider}:${item.projectId}:${version}`;
}

export function initialDownloadCards(
  items: readonly DownloadCardIdentity[],
): DownloadCardMap {
  const cards: DownloadCardMap = {};
  for (const item of items) {
    cards[downloadCardKey(item)] = {
      status: 'queued',
      downloaded: 0,
      total: item.size && item.size > 0 ? Math.floor(item.size) : null,
    };
  }
  return cards;
}

function sanitizeBytes(value: number): number {
  return Number.isFinite(value) && value > 0 ? Math.floor(value) : 0;
}

/** Mark one card as actively downloading; queued items keep their state. */
export function startCard(
  cards: DownloadCardMap,
  key: string,
  expectedSize?: number | null,
): DownloadCardMap {
  const previous = cards[key];
  if (!previous) return cards;
  return {
    ...cards,
    [key]: {
      status: 'downloading',
      downloaded: 0,
      total: expectedSize && expectedSize > 0 ? Math.floor(expectedSize) : null,
    },
  };
}

/** Apply an absolute byte event to one card only. */
export function applyCardChunk(
  cards: DownloadCardMap,
  key: string,
  downloaded: number,
  total: number | null,
): DownloadCardMap {
  const previous = cards[key];
  if (!previous) return cards;
  return {
    ...cards,
    [key]: {
      status: 'downloading',
      downloaded: sanitizeBytes(downloaded),
      // A chunk event without Content-Length must not discard a size already
      // known from the catalog; it only stays unknown when both are absent.
      total: total != null && total > 0 ? Math.floor(total) : (previous.total ?? null),
    },
  };
}

export function completeCard(cards: DownloadCardMap, key: string): DownloadCardMap {
  const previous = cards[key];
  if (!previous) return cards;
  const total = previous.total ?? (previous.downloaded > 0 ? previous.downloaded : null);
  return {
    ...cards,
    [key]: {
      status: 'done',
      downloaded: total ?? previous.downloaded,
      total,
    },
  };
}

export function failCard(cards: DownloadCardMap, key: string): DownloadCardMap {
  const previous = cards[key];
  if (!previous) return cards;
  return {
    ...cards,
    [key]: { ...previous, status: 'error' },
  };
}

/** Card percent: null while a download with an unknown size is in flight. */
export function cardPercent(state: DownloadCardState | undefined): number | null {
  if (!state) return null;
  if (state.status === 'done') return 100;
  if (state.status === 'queued') return 0;
  return fileProgressPercent(state.downloaded, state.total);
}

export function cardIsIndeterminate(state: DownloadCardState | undefined): boolean {
  return state?.status === 'downloading' && (state.total == null || state.total <= 0);
}

/**
 * Overall percent for the whole operation. Known sizes are weighted by bytes;
 * any unknown size falls back to completed-file counting. Only a fully
 * successful operation reaches 100%.
 */
export function overallDownloadPercent(cards: DownloadCardMap): number | null {
  const list = Object.values(cards);
  if (list.length === 0) return null;
  const allDone = list.every((card) => card.status === 'done');
  const allSizesKnown = list.every((card) => card.total != null && card.total > 0);

  if (allSizesKnown) {
    const totalBytes = list.reduce((sum, card) => sum + (card.total ?? 0), 0);
    const downloaded = list.reduce(
      (sum, card) => sum + Math.min(card.downloaded, card.total ?? card.downloaded),
      0,
    );
    return weightedProgressPercent(downloaded, 0, totalBytes, allDone);
  }

  const completed = list.filter((card) => card.status === 'done').length;
  return countProgressPercent(completed, list.length, allDone);
}
