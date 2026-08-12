import { useCallback, useEffect, useRef } from 'react';

interface SliderProps {
  min: number;
  max: number;
  step: number;
  value: number;
  onChange: (value: number) => void;
  disabled?: boolean;
}

const clamp = (v: number, min: number, max: number) => Math.min(max, Math.max(min, v));

export function Slider({ min, max, step, value, onChange, disabled }: SliderProps) {
  const trackRef = useRef<HTMLDivElement>(null);
  const draggingRef = useRef(false);

  const snap = (v: number) => Math.round((v - min) / step) * step + min;
  const safeMax = Math.max(min + step, max);
  const current = snap(clamp(value, min, safeMax));
  const span = safeMax - min;
  const pct = (span <= 0 ? 0 : ((current - min) / span) * 100);

  const valueFromClientX = useCallback((clientX: number) => {
    const el = trackRef.current;
    if (!el) return current;
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0) return current;
    const ratio = clamp((clientX - rect.left) / rect.width, 0, 1);
    return snap(min + ratio * span);
  }, [min, span, current]);

  useEffect(() => {
    if (disabled) return;
    const onMove = (e: PointerEvent) => {
      if (!draggingRef.current) return;
      onChange(valueFromClientX(e.clientX));
    };
    const onUp = () => {
      draggingRef.current = false;
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    return () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
    };
  }, [disabled, onChange, valueFromClientX]);

  return (
    <div
      ref={trackRef}
      className={`slider${disabled ? ' slider--disabled' : ''}`}
      role="slider"
      aria-valuemin={min}
      aria-valuemax={safeMax}
      aria-valuenow={current}
      aria-label="slider"
      tabIndex={disabled ? -1 : 0}
      onPointerDown={(e) => {
        if (disabled || e.button !== 0) return;
        draggingRef.current = true;
        try {
          (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
        } catch {
          // capture is optional
        }
        onChange(valueFromClientX(e.clientX));
      }}
      onKeyDown={(e) => {
        if (disabled) return;
        const dir =
          e.key === 'ArrowRight' || e.key === 'ArrowUp' ? step
          : e.key === 'ArrowLeft' || e.key === 'ArrowDown' ? -step
          : 0;
        if (dir === 0) return;
        e.preventDefault();
        onChange(snap(clamp(current + dir, min, safeMax)));
      }}
    >
      <div className="slider__track">
        <div className="slider__fill" style={{ width: `${pct}%` }} />
      </div>
      <div className="slider__thumb" style={{ left: `${pct}%` }} />
    </div>
  );
}