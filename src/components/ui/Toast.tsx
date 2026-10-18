import { useEffect, useState } from 'react';
import { X, CheckCircle, AlertCircle, AlertTriangle, Info } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { logUiMessage, toastTypeToLogLevel } from '../../lib/uiLog';

type ToastType = 'success' | 'error' | 'warning' | 'info';

interface ToastData {
  id: string;
  message: string;
  type: ToastType;
  /** How many times this exact toast arrived while it was on screen. */
  count: number;
}

interface ToastItemProps {
  toast: ToastData;
  onDismiss: (id: string) => void;
}

const icons: Record<ToastType, React.ReactNode> = {
  success: <CheckCircle size={16} color="var(--success)" />,
  error: <AlertCircle size={16} color="var(--error)" />,
  warning: <AlertTriangle size={16} color="var(--warning)" />,
  info: <Info size={16} color="var(--info)" />,
};

const TOAST_DURATION_MS = 4000;

function ToastItem({ toast, onDismiss }: ToastItemProps) {
  const [exiting, setExiting] = useState(false);

  // `toast.count` in the deps restarts the lifetime whenever an identical
  // toast arrives, so the (2)/(3) counter stays readable for a full duration.
  useEffect(() => {
    setExiting(false);
    let fadeTimer: ReturnType<typeof setTimeout> | undefined;
    const hideTimer = setTimeout(() => {
      setExiting(true);
      fadeTimer = setTimeout(() => onDismiss(toast.id), 300);
    }, TOAST_DURATION_MS);
    return () => {
      clearTimeout(hideTimer);
      clearTimeout(fadeTimer);
    };
  }, [toast.id, toast.count, onDismiss]);

  return (
    <div
      className={`toast toast--${toast.type}`}
      style={{
        ['--toast-duration' as string]: `${TOAST_DURATION_MS}ms`,
        // Alternating the progress-bar keyframe name restarts the CSS
        // animation when the counter bumps — keeping the visual depletion in
        // step with the restarted JS lifetime above. Sending the same name
        // back does NOT restart a running animation, hence the A/B twins.
        ['--toast-anim' as string]: toast.count % 2 === 0 ? 'toast-life' : 'toast-life-b',
        animation: exiting ? 'fadeOut 0.3s ease-out forwards' : undefined,
      }}
    >
      {icons[toast.type]}
      <span style={{ flex: 1 }}>
        {toast.message}
        {toast.count > 1 ? ` (${toast.count})` : ''}
      </span>
      <button
        className="btn btn--ghost btn--sm"
        onClick={() => onDismiss(toast.id)}
        style={{ padding: 2, height: 'auto' }}
      >
        <X size={14} />
      </button>
    </div>
  );
}

// Global toast state
let toastListeners: Array<(toasts: ToastData[]) => void> = [];
let toasts: ToastData[] = [];
let idCounter = 0;

/**
 * Show a toast. An identical one (same text AND type) that is still on
 * screen is NOT duplicated into a second bubble: its counter bumps — the
 * text becomes "… (2)", "… (3)" — and its lifetime restarts, so the latest
 * occurrence always gets a full reading time. A NEW toast appears when the
 * previous identical one has already faded away.
 */
export function addToast(message: string, type: ToastType = 'info') {
  const existing = toasts.find((t) => t.message === message && t.type === type);
  if (existing) {
    toasts = toasts.map((t) =>
      t.id === existing.id ? { ...t, count: t.count + 1 } : t
    );
  } else {
    const id = `toast-${++idCounter}`;
    toasts = [...toasts, { id, message, type, count: 1 }];
  }
  toastListeners.forEach((l) => l(toasts));
  invoke('cmd_log_toast', { level: type, message }).catch(() => {
    logUiMessage(message, toastTypeToLogLevel(type), 'toast');
  });
}

export function dismissToast(id: string) {
  toasts = toasts.filter((t) => t.id !== id);
  toastListeners.forEach((l) => l(toasts));
}

export function ToastContainer() {
  const [items, setItems] = useState<ToastData[]>([]);

  useEffect(() => {
    toastListeners.push(setItems);
    return () => {
      toastListeners = toastListeners.filter((l) => l !== setItems);
    };
  }, []);

  if (items.length === 0) return null;

  // `dismissToast` is a stable module function: passing it (not a closure
  // re-created on every render) keeps the items' lifetime effect from
  // restarting on unrelated re-renders of the app — otherwise a busy
  // re-rendering parent would keep toasts alive forever.
  return (
    <div className="toast-container">
      {items.map((t) => (
        <ToastItem key={t.id} toast={t} onDismiss={dismissToast} />
      ))}
    </div>
  );
}
