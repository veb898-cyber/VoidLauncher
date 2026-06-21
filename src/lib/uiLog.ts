import { useEffect, useRef } from 'react';
import { useLogStore, type LogEntry } from '../stores/logStore';

export type UiLogLevel = LogEntry['level'];

/** Write a UI notification to the launcher Logs tab. */
export function logUiMessage(message: string, level: UiLogLevel = 'info', source = 'ui') {
  const trimmed = message.trim();
  if (!trimmed) return;
  useLogStore.getState().addLog({ level, source, message: trimmed });
}

export function toastTypeToLogLevel(type: 'success' | 'error' | 'warning' | 'info'): UiLogLevel {
  switch (type) {
    case 'error':
      return 'error';
    case 'warning':
      return 'warn';
    default:
      return 'info';
  }
}

/** Log when an inline banner/message becomes visible (dedupes identical text). */
export function useLogPlaque(
  message: string | null | undefined,
  level: UiLogLevel,
  source: string,
) {
  const lastLogged = useRef<string | null>(null);

  useEffect(() => {
    const trimmed = message?.trim();
    if (!trimmed) {
      lastLogged.current = null;
      return;
    }
    if (trimmed === lastLogged.current) return;
    logUiMessage(trimmed, level, source);
    lastLogged.current = trimmed;
  }, [message, level, source]);
}
