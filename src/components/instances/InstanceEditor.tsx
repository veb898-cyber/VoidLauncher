import { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { Button } from '../ui/Button';
import { Input } from '../ui/Input';
import { Modal } from '../ui/Modal';
import { addToast } from '../ui/Toast';
import { CustomSelect, type SelectOption } from '../ui/CustomSelect';
import {
  MIN_MEMORY_MB,
  MEMORY_STEP_MB,
  getMaxMemoryMb,
  resolveInitialMemoryMb,
} from '../../lib/memory';
import { useSettingsStore } from '../../stores/settingsStore';
import { useT } from '../../lib/i18n';

interface InstanceData {
  name: string;
  mc_version: string;
  loader: string;
  loader_version?: string | null;
  memory_mb?: number | null;
  jvm_args?: string[] | null;
  gc_preset?: string | null;
  java_path?: string | null;
  resolution?: { width: number; height: number } | null;
  icon?: string | null;
  notes: string;
}

interface Props {
  open: boolean;
  instance: InstanceData;
  onClose: () => void;
  onSaved: () => void;
}

type GcPreset = 'standard' | 'g1gc' | 'zgc';

export function InstanceEditor({ open, instance, onClose, onSaved }: Props) {
  const t = useT();
  const presetOptions: SelectOption<GcPreset>[] = [
    { value: 'standard', label: t('instance_editor.gc_standard'), description: 'No special GC flags' },
    { value: 'g1gc', label: t('instance_editor.gc_g1gc'), description: 'Java 8+' },
    { value: 'zgc', label: t('instance_editor.gc_zgc'), description: 'Java 17+, ≥6 GB' },
  ];
  const globalConfig = useSettingsStore((s) => s.config);
  const globalDefaultMemoryMb = globalConfig?.default_memory_mb ?? null;
  const globalDefaultGcPreset = (globalConfig?.default_gc_preset as GcPreset) ?? 'g1gc';

  const [name, setName] = useState(instance.name);
  const [notes, setNotes] = useState(instance.notes || '');
  const [memoryMb, setMemoryMb] = useState<number>(
    instance.memory_mb ?? globalDefaultMemoryMb ?? 4096
  );
  const [jvmArgs, setJvmArgs] = useState(instance.jvm_args?.join(' ') || '');
  const [gcPreset, setGcPreset] = useState<GcPreset>(
    (instance.gc_preset as GcPreset) ?? globalDefaultGcPreset
  );
  const [javaPath, setJavaPath] = useState(instance.java_path || '');
  const [resWidth, setResWidth] = useState(instance.resolution?.width?.toString() || '');
  const [resHeight, setResHeight] = useState(instance.resolution?.height?.toString() || '');
  const [systemRamMb, setSystemRamMb] = useState<number>(8192);

  useEffect(() => {
    setName(instance.name);
    setNotes(instance.notes || '');
    // Initial memory resolves in this order:
    //   1. instance.memory_mb (explicit per-instance value)
    //   2. global default_memory_mb from Settings (synced)
    //   3. tiered RAM recommendation (4/6/8 GB) as final fallback
    setMemoryMb(
      resolveInitialMemoryMb(instance.memory_mb, systemRamMb, globalDefaultMemoryMb)
    );
    setJvmArgs(instance.jvm_args?.join(' ') || '');
    setGcPreset((instance.gc_preset as GcPreset) ?? globalDefaultGcPreset);
    setJavaPath(instance.java_path || '');
    setResWidth(instance.resolution?.width?.toString() ?? '');
    setResHeight(instance.resolution?.height?.toString() ?? '');
  }, [instance, systemRamMb, globalDefaultMemoryMb, globalDefaultGcPreset]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    (async () => {
      try {
        const total = await invoke<number>('cmd_detect_system_ram');
        if (!cancelled) setSystemRamMb(total);
      } catch {
        /* fall back to default 8192 */
      }
    })();
    return () => { cancelled = true; };
  }, [open]);

  // Dynamic max memory: min(total - 2 GB, 80% of total), floored to step, with a hard minimum of MIN_MEMORY_MB
  const maxMemoryMb = useMemo(
    () => getMaxMemoryMb(systemRamMb),
    [systemRamMb]
  );

  // Clamp the memory value when bounds change (e.g., system RAM detected after open)
  useEffect(() => {
    setMemoryMb((m) => Math.min(Math.max(m, MIN_MEMORY_MB), maxMemoryMb));
  }, [maxMemoryMb]);

  const handleSave = async () => {
    try {
      const updated = {
        ...instance,
        name,
        notes,
        memory_mb: memoryMb,
        jvm_args: jvmArgs.trim() ? jvmArgs.split(/\s+/).filter(Boolean) : null,
        gc_preset: gcPreset,
        java_path: javaPath.trim() || null,
        resolution: (() => {
          // parseInt('') returns NaN; parseInt('12px') returns 12 (silent
          // truncation). Sanitize both fields and only persist when both
          // parse to a positive integer in the [1, 32768] range.
          const w = Number.parseInt(resWidth, 10);
          const h = Number.parseInt(resHeight, 10);
          if (!Number.isFinite(w) || !Number.isFinite(h)) return null;
          if (w < 1 || w > 32768 || h < 1 || h > 32768) return null;
          return { width: w, height: h };
        })(),
      };
      await invoke('cmd_save_instance', { instance: updated });
      addToast(t('instance_editor.saved_toast'), 'success');
      onSaved();
      onClose();
    } catch (e: any) {
      addToast(t('instance_editor.save_error', { error: e.toString() }), 'error');
    }
  };

  const pickJavaPath = async () => {
    const selected = await openFileDialog({
      title: t('instance_editor.java_file_title'),
      filters: [{ name: 'Java', extensions: ['exe'] }],
      multiple: false,
    });
    if (selected) setJavaPath(selected);
  };

  const systemRamGb = (systemRamMb / 1024).toFixed(1);
  const allocatedGb = (memoryMb / 1024).toFixed(1);
  const maxGb = (maxMemoryMb / 1024).toFixed(1);
  const zgcDisabled = memoryMb < 6144;

  return (
    <Modal open={open} onClose={onClose} title={t('instance_editor.title')}>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
        <Input label={t('instance_editor.name_label')} id="edit-name" type="text" value={name} onChange={(e) => setName(e.target.value)} />
        <Input label={t('instance_editor.notes_label')} id="edit-notes" type="text" value={notes} onChange={(e) => setNotes(e.target.value)} />

        {/* Memory slider with dynamic max */}
        <div>
          <label className="input__label" style={{ display: 'block', marginBottom: 6 }}>
            {t('instance_editor.memory_label')} <strong>{allocatedGb} GB</strong>
            <span style={{ color: 'var(--text-tertiary)', marginLeft: 8, fontSize: 'var(--font-size-xs)' }}>
              {t('instance_editor.memory_max_info', { maxGb, systemRamGb })}
            </span>
          </label>
          <input
            type="range"
            min={MIN_MEMORY_MB}
            max={maxMemoryMb}
            step={MEMORY_STEP_MB}
            value={memoryMb}
            onChange={(e) => setMemoryMb(parseInt(e.target.value))}
            style={{ width: '100%' }}
          />
          <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: 2 }}>
            <span>{t('instance_editor.memory_min_label', { gb: (MIN_MEMORY_MB / 1024).toFixed(1) })}</span>
            <span>{t('instance_editor.memory_max_label', { maxGb })}</span>
          </div>
          {globalDefaultMemoryMb && globalDefaultMemoryMb !== memoryMb && (
            <div style={{ marginTop: 6 }}>
              <button
                type="button"
                className="btn btn--ghost btn--sm"
                onClick={() => setMemoryMb(globalDefaultMemoryMb)}
                title={`Set to global default (${(globalDefaultMemoryMb / 1024).toFixed(1)} GB)`}
              >
                {t('instance_editor.memory_reset_to_global')}
              </button>
            </div>
          )}
        </div>

        {/* GC preset dropdown */}
        <div>
          <label className="input__label" style={{ display: 'block', marginBottom: 4 }}>
            {t('instance_editor.gc_label')}
          </label>
          <CustomSelect<GcPreset>
            value={gcPreset}
            options={presetOptions.map((o) =>
              o.value === 'zgc' && zgcDisabled ? { ...o, disabled: true } : o
            )}
            onChange={setGcPreset}
          />
          <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: 4 }}>
            {gcPreset === 'standard' && t('instance_editor.gc_standard_desc')}
            {gcPreset === 'g1gc' && t('instance_editor.gc_g1gc_desc')}
            {gcPreset === 'zgc' && (
              zgcDisabled
                ? <span style={{ color: 'var(--color-warning)' }}>{t('instance_editor.gc_zgc_unavailable', { allocatedGb })}</span>
                : <>{t('instance_editor.gc_zgc_desc')} <span style={{ color: 'var(--color-warning)' }}>{t('instance_editor.gc_zgc_warning')}</span></>
            )}
          </div>
        </div>

        <Input
          label={t('instance_editor.jvm_label')}
          id="edit-jvm"
          type="text"
          value={jvmArgs}
          onChange={(e) => setJvmArgs(e.target.value)}
          placeholder={t('instance_editor.jvm_placeholder')}
        />

        <div>
          <label className="input__label" style={{ display: 'block', marginBottom: 4 }}>{t('instance_editor.java_path_label')}</label>
          <div style={{ display: 'flex', gap: 'var(--space-xs)' }}>
            <input className="input" type="text" value={javaPath} onChange={(e) => setJavaPath(e.target.value)} placeholder={t('instance_editor.java_placeholder')} style={{ flex: 1 }} />
            <Button size="sm" variant="ghost" onClick={pickJavaPath}>{t('instance_editor.browse_btn')}</Button>
          </div>
        </div>
        <div>
          <label className="input__label" style={{ display: 'block', marginBottom: 4 }}>{t('instance_editor.resolution_label')}</label>
          <div style={{ display: 'flex', gap: 'var(--space-xs)', alignItems: 'center' }}>
            <input className="input" type="number" value={resWidth} onChange={(e) => setResWidth(e.target.value)} placeholder={t('instance_editor.width_placeholder')} style={{ flex: 1 }} />
            <span style={{ color: 'var(--text-tertiary)' }}>×</span>
            <input className="input" type="number" value={resHeight} onChange={(e) => setResHeight(e.target.value)} placeholder={t('instance_editor.height_placeholder')} style={{ flex: 1 }} />
          </div>
        </div>
      </div>
      <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
        <Button variant="ghost" onClick={onClose}>{t('common.cancel')}</Button>
        <Button onClick={handleSave}>{t('common.save')}</Button>
      </div>
    </Modal>
  );
}
