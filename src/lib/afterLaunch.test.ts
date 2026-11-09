import { describe, expect, it, vi } from 'vitest';
import { afterLaunchWindowAction, applyAfterLaunchAction } from './afterLaunch';

const window = () => ({ minimize: vi.fn().mockResolvedValue(undefined), close: vi.fn().mockResolvedValue(undefined) });

describe('after-launch window action', () => {
  it('does nothing for a failed or still-starting launch', () => {
    expect(afterLaunchWindowAction('starting', 'minimize')).toBe('none');
    expect(afterLaunchWindowAction('error', 'minimize')).toBe('none');
    expect(afterLaunchWindowAction('exited', 'close')).toBe('none');
  });

  it('maps the three saved choices for a confirmed running game', () => {
    expect(afterLaunchWindowAction('running', 'do_nothing')).toBe('none');
    expect(afterLaunchWindowAction('running', 'minimize')).toBe('minimize');
    expect(afterLaunchWindowAction('running', 'close')).toBe('close');
    expect(afterLaunchWindowAction('running', undefined)).toBe('none');
  });

  it('calls only the selected window method', async () => {
    const w = window();
    await expect(applyAfterLaunchAction('none', w)).resolves.toBe('none');
    expect(w.minimize).not.toHaveBeenCalled();
    expect(w.close).not.toHaveBeenCalled();

    await expect(applyAfterLaunchAction('minimize', w)).resolves.toBe('minimize');
    expect(w.minimize).toHaveBeenCalledTimes(1);
    expect(w.close).not.toHaveBeenCalled();

    await expect(applyAfterLaunchAction('close', w)).resolves.toBe('close');
    expect(w.close).toHaveBeenCalledTimes(1);
    expect(w.minimize).toHaveBeenCalledTimes(1);
  });
});
