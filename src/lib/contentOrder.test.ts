import { describe, expect, it } from 'vitest';
import { moveActiveContent, resourcePacksOptionsOrder } from './contentOrder';

const pack = (filename: string, enabled = true) => ({ filename, enabled });

describe('moveActiveContent', () => {
  it('moves an active pack up and down while skipping disabled packs', () => {
    const items = [pack('High.zip'), pack('Middle.zip'), pack('Disabled.zip', false), pack('Low.zip')];

    const up = moveActiveContent(items, 'Low.zip', 'up');
    expect(up?.map((item) => item.filename)).toEqual([
      'High.zip',
      'Low.zip',
      'Middle.zip',
      'Disabled.zip',
    ]);

    const down = moveActiveContent(items, 'High.zip', 'down');
    expect(down?.map((item) => item.filename)).toEqual([
      'Middle.zip',
      'High.zip',
      'Low.zip',
      'Disabled.zip',
    ]);
  });

  it('returns null at the edges, for disabled items, and for unknown names', () => {
    const items = [pack('A.zip'), pack('B.zip'), pack('C.zip', false)];

    expect(moveActiveContent(items, 'A.zip', 'up')).toBeNull();
    expect(moveActiveContent(items, 'B.zip', 'down')).toBeNull();
    expect(moveActiveContent(items, 'C.zip', 'up')).toBeNull();
    expect(moveActiveContent(items, 'missing.zip', 'down')).toBeNull();
  });
});

describe('resourcePacksOptionsOrder', () => {
  it('reverses display order and omits disabled packs', () => {
    const items = [pack('High.zip'), pack('Middle.zip'), pack('Disabled.zip', false), pack('Low.zip')];
    expect(resourcePacksOptionsOrder(items)).toEqual(['Low.zip', 'Middle.zip', 'High.zip']);
  });
});
