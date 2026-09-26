import { type ReactNode, useEffect, useRef } from 'react';

interface ModalProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  children: ReactNode;
  footer?: ReactNode;
  maxWidth?: number;
  /**
   * `bare` removes the dark, blurred backdrop. The dialog still floats
   * centered and click-outside / Escape still close it, but the page
   * behind remains fully visible and interactive-look (no dim, no blur).
   * Used by the Microsoft login card so the in-page dialog matches the
   * standalone "ВОЙТИ" page exactly.
   */
  bare?: boolean;
  /**
   * `fitContent` sizes the dialog to its content instead of capping it at
   * 85vh with an internal scrollbar. The dialog still falls back to a
   * scrollbar when it would exceed the viewport (max-height caps at the
   * viewport minus breathing room), so nothing gets clipped on short
   * windows. Used by the instance editor whose settings fit on screen.
   */
  fitContent?: boolean;
  /**
   * `growWithContent` lets the dialog take exactly the width its content needs
   * instead of a fixed one. Because the overlay centres the dialog, a wider
   * dialog grows half to the left and half to the right, so nothing ever sticks
   * out past the right edge of an otherwise fixed-width window.
   */
  growWithContent?: boolean;
}

export function Modal({ open, onClose, title, children, footer, maxWidth, bare, fitContent, growWithContent }: ModalProps) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const wasOpen = useRef(false);

  // Focus management tied to the `open` flag only: on show, move focus into
  // the dialog (without stealing it from an already-focused element inside,
  // e.g. an autoFocus input); on hide, restore focus to whatever had it
  // before the dialog opened.
  useEffect(() => {
    if (open && !wasOpen.current) {
      wasOpen.current = true;
      const previouslyFocused = document.activeElement as HTMLElement | null;
      requestAnimationFrame(() => {
        const el = dialogRef.current;
        if (el && !el.contains(document.activeElement)) {
          el.focus({ preventScroll: true });
        }
      });
      return () => {
        wasOpen.current = false;
        if (previouslyFocused && typeof previouslyFocused.focus === 'function') {
          try { previouslyFocused.focus({ preventScroll: true }); } catch { /* ignore */ }
        }
      };
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className={bare ? 'modal-overlay modal-overlay--bare' : 'modal-overlay'}
      onClick={onClose}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={title || undefined}
        tabIndex={-1}
        className={`modal animate-slide-up${fitContent ? ' modal--fit' : ''}${growWithContent ? ' modal--content' : ''}`}
        onClick={(e) => e.stopPropagation()}
        style={maxWidth ? { maxWidth } : undefined}
      >
        {title && (
          <div className="modal__header">
            <h2 className="modal__title">{title}</h2>
          </div>
        )}
        <div className="modal__body">{children}</div>
        {footer && <div className="modal__footer">{footer}</div>}
      </div>
    </div>
  );
}
