import { describe, expect, it } from 'vitest';
import { en } from './i18n/en';
import { ru } from './i18n/ru';
import { labelWithInstance } from './accessibleLabel';

describe('labelWithInstance', () => {
  it('uses only the action label when no instance is selected', () => {
    expect(labelWithInstance(en['game_logs.runs'], null)).toBe(en['game_logs.runs']);
    expect(labelWithInstance(en['game_logs.runs'], '   ')).toBe(en['game_logs.runs']);
  });

  it('appends the selected instance in both languages', () => {
    expect(labelWithInstance(en['game_logs.runs'], 'Alpha Pack')).toBe(
      `${en['game_logs.runs']}: Alpha Pack`,
    );
    expect(labelWithInstance(ru['game_logs.runs'], 'Сборка')).toBe(
      `${ru['game_logs.runs']}: Сборка`,
    );
  });
});
