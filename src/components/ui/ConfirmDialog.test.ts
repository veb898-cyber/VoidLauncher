import { createElement, type ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';
import { useLanguageStore } from '../../stores/languageStore';

// The real Modal renders through a portal and needs a DOM. This test only
// exercises ConfirmDialog's own variant decision, so the shell is replaced by
// a plain wrapper that still renders the footer with its Buttons.
vi.mock('./Modal', async () => {
  const React = await import('react');
  return {
    Modal: ({ footer }: { footer?: ReactNode }) =>
      React.createElement('div', { 'data-testid': 'modal' }, footer),
  };
});

import { ConfirmDialog } from './ConfirmDialog';

function render(variant?: 'danger' | 'primary'): string {
  useLanguageStore.setState({ language: 'en' });
  return renderToStaticMarkup(
    createElement(ConfirmDialog, {
      open: true,
      title: 'Confirm',
      description: 'Proceed?',
      confirmLabel: 'Proceed',
      onConfirm: () => {},
      onCancel: () => {},
      ...(variant ? { variant } : {}),
    }),
  );
}

describe('ConfirmDialog variant', () => {
  it('uses the primary action by default for ordinary confirmations', () => {
    const html = render();
    expect(html).toContain('btn--primary');
    expect(html).not.toContain('btn--danger');
  });

  it('uses the danger action for destructive confirmations', () => {
    const html = render('danger');
    expect(html).toContain('btn--danger');
    expect(html).not.toContain('btn--primary');
  });

  it('keeps an explicit primary variant for non-destructive confirmations', () => {
    const html = render('primary');
    expect(html).toContain('btn--primary');
    expect(html).not.toContain('btn--danger');
  });
});
