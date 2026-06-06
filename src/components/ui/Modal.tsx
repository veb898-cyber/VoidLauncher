import { type ReactNode, useEffect } from 'react';

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
}

export function Modal({ open, onClose, title, children, footer, maxWidth, bare }: ModalProps) {
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
        className="modal animate-slide-up"
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
