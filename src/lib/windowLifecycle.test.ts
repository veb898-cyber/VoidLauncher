import { describe, expect, it } from 'vitest';
import capabilities from '../../src-tauri/capabilities/default.json?raw';
import app from '../App.tsx?raw';
import titlebar from '../components/Titlebar.tsx?raw';
import libRs from '../../src-tauri/src/lib.rs?raw';

/**
 * Static regression checks for the window lifecycle. These do not replace the
 * Windows runtime test, but they make the two permission regressions and the
 * close-guard wiring impossible to remove silently.
 */
describe('window lifecycle', () => {
  it('grants toggle-maximize and destroy while keeping the close guard', () => {
    expect(capabilities).toContain('"core:window:allow-toggle-maximize"');
    expect(capabilities).toContain('"core:window:allow-destroy"');
    expect(capabilities).toContain('"core:window:allow-close"');

    expect(app).toContain('onCloseRequested');
    expect(app).toContain('event.preventDefault()');
    expect(app).toContain('guard.askUnsaved');
    expect(app).toContain('appWindow.destroy()');
  });

  it('tracks the real maximized state from resize events', () => {
    expect(titlebar).toContain('onResized');
    expect(titlebar).toContain('isMaximized');
    expect(titlebar).toContain("t('titlebar.restore')");
    expect(titlebar).toContain("t('titlebar.maximize')");
  });

  it('flushes playtime only when the application exits', () => {
    expect(libRs).toContain('RunEvent::Exit');
    expect(libRs).toContain('flush_playtime_on_exit');
    expect(libRs).not.toContain('WindowEvent::CloseRequested');
  });
});
