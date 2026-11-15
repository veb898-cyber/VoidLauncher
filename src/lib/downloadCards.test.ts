import { describe, expect, it } from 'vitest';
import {
  applyCardChunk,
  cardIsIndeterminate,
  cardPercent,
  completeCard,
  downloadCardKey,
  failCard,
  initialDownloadCards,
  overallDownloadPercent,
  startCard,
} from './downloadCards';

const items = [
  { source: 'modrinth', projectId: 'sodium', versionId: 'v1', filename: 'sodium.jar' },
  { source: 'modrinth', projectId: 'lithium', versionId: 'v2', filename: 'lithium.jar' },
];

const keyOf = (projectId: string) =>
  downloadCardKey(items.find((i) => i.projectId === projectId)!);

describe('per-card download state', () => {
  it('starts every queued card and updates only the active file', () => {
    let cards = initialDownloadCards(items);
    expect(cardPercent(cards[keyOf('sodium')])).toBe(0);

    cards = startCard(cards, keyOf('sodium'), 1000);
    cards = applyCardChunk(cards, keyOf('sodium'), 400, 1000);

    expect(cards[keyOf('sodium')].downloaded).toBe(400);
    expect(cards[keyOf('sodium')].status).toBe('downloading');
    expect(cards[keyOf('lithium')]).toEqual({
      status: 'queued',
      downloaded: 0,
      total: null,
    });
    expect(cardPercent(cards[keyOf('sodium')])).toBe(40);
  });

  it('keeps parallel cards independent', () => {
    let cards = initialDownloadCards(items);
    cards = startCard(cards, keyOf('sodium'), 1000);
    cards = startCard(cards, keyOf('lithium'), 500);
    cards = applyCardChunk(cards, keyOf('sodium'), 200, 1000);
    cards = applyCardChunk(cards, keyOf('lithium'), 300, 500);

    expect(cardPercent(cards[keyOf('sodium')])).toBe(20);
    expect(cardPercent(cards[keyOf('lithium')])).toBe(60);
  });

  it('completes one card without touching the others', () => {
    let cards = initialDownloadCards(items);
    cards = startCard(cards, keyOf('sodium'), 1000);
    cards = applyCardChunk(cards, keyOf('sodium'), 1000, 1000);
    cards = completeCard(cards, keyOf('sodium'));

    expect(cards[keyOf('sodium')].status).toBe('done');
    expect(cardPercent(cards[keyOf('sodium')])).toBe(100);
    expect(cards[keyOf('lithium')].status).toBe('queued');
  });

  it('keeps an unknown-size download indeterminate until it finishes', () => {
    let cards = initialDownloadCards([items[0]]);
    const key = keyOf('sodium');
    cards = startCard(cards, key, null);
    cards = applyCardChunk(cards, key, 512, null);

    expect(cardPercent(cards[key])).toBeNull();
    expect(cardIsIndeterminate(cards[key])).toBe(true);

    cards = completeCard(cards, key);
    expect(cards[key].status).toBe('done');
    expect(cardPercent(cards[key])).toBe(100);
  });

  it('marks only the failed card as error', () => {
    let cards = initialDownloadCards(items);
    cards = startCard(cards, keyOf('sodium'), 1000);
    cards = startCard(cards, keyOf('lithium'), 500);
    cards = applyCardChunk(cards, keyOf('sodium'), 700, 1000);
    cards = failCard(cards, keyOf('sodium'));

    expect(cards[keyOf('sodium')].status).toBe('error');
    expect(cards[keyOf('sodium')].downloaded).toBe(700);
    expect(cards[keyOf('sodium')].status).not.toBe('done');
    expect(cards[keyOf('lithium')].status).toBe('downloading');
    expect(overallDownloadPercent(cards)).not.toBe(100);
  });

  it('does not add bytes from a retried attempt', () => {
    let cards = initialDownloadCards([items[0]]);
    const key = keyOf('sodium');
    cards = startCard(cards, key, 1000);
    cards = applyCardChunk(cards, key, 600, 1000);
    cards = applyCardChunk(cards, key, 0, 1000);
    cards = applyCardChunk(cards, key, 250, 1000);

    expect(cards[key].downloaded).toBe(250);
  });

  it('weights the overall bar by bytes, not by card count', () => {
    let cards = initialDownloadCards(items);
    // Sodium is 100 bytes, lithium is 900 bytes.
    cards = startCard(cards, keyOf('sodium'), 100);
    cards = startCard(cards, keyOf('lithium'), 900);
    cards = applyCardChunk(cards, keyOf('sodium'), 100, 100);
    cards = completeCard(cards, keyOf('sodium'));
    cards = applyCardChunk(cards, keyOf('lithium'), 450, 900);

    expect(overallDownloadPercent(cards)).toBeCloseTo(55, 5);
    cards = failCard(cards, keyOf('lithium'));
    expect(overallDownloadPercent(cards)).not.toBe(100);
  });

  it('reaches 100 only when every card is done', () => {
    let cards = initialDownloadCards(items);
    cards = startCard(cards, keyOf('sodium'), 100);
    cards = startCard(cards, keyOf('lithium'), 900);
    cards = applyCardChunk(cards, keyOf('sodium'), 100, 100);
    cards = completeCard(cards, keyOf('sodium'));
    expect(overallDownloadPercent(cards)).not.toBe(100);

    cards = applyCardChunk(cards, keyOf('lithium'), 900, 900);
    cards = completeCard(cards, keyOf('lithium'));
    expect(overallDownloadPercent(cards)).toBe(100);
  });

  it('uses a count fallback when sizes are unknown', () => {
    let cards = initialDownloadCards(items);
    cards = startCard(cards, keyOf('sodium'), null);
    cards = startCard(cards, keyOf('lithium'), null);
    cards = applyCardChunk(cards, keyOf('sodium'), 50, null);
    cards = completeCard(cards, keyOf('sodium'));

    expect(overallDownloadPercent(cards)).toBe(50);
    cards = applyCardChunk(cards, keyOf('lithium'), 10, null);
    cards = completeCard(cards, keyOf('lithium'));
    expect(overallDownloadPercent(cards)).toBe(100);
  });
});
