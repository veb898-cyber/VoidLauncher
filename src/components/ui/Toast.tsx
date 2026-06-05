import { useEffect, useState } from 'react';
import { X, CheckCircle, AlertCircle, AlertTriangle, Info } from 'lucide-react';

type ToastType = 'success' | 'error' | 'warning' | 'info';

interface ToastData {
  id: string;
  message: string;
  type: ToastType;
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

function ToastItem({ toast, onDismiss }: ToastItemProps) {
  const [exiting, setExiting] = useState(false);

  useEffect(() => {
    const timer = setTimeout(() => {
      setExiting(true);
      setTimeout(() => onDismiss(toast.id), 300);
    }, 4000);
    return () => clearTimeout(timer);
  }, [toast.id, onDismiss]);

  return (
    <div
      className={`toast toast--${toast.type}`}
      style={{
        animation: exiting ? 'fadeOut 0.3s ease-out forwards' : undefined,
      }}
    >
      {icons[toast.type]}
      <span style={{ flex: 1 }}>{toast.message}</span>
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

export function addToast(message: string, type: ToastType = 'info') {
  const id = `toast-${++idCounter}`;
  toasts = [...toasts, { id, message, type }];
  toastListeners.forEach((l) => l(toasts));
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

  const handleDismiss = (id: string) => {
    dismissToast(id);
  };

  if (items.length === 0) return null;

  return (
    <div className="toast-container">
      {items.map((t) => (
        <ToastItem key={t.id} toast={t} onDismiss={handleDismiss} />
      ))}
    </div>
  );
}
