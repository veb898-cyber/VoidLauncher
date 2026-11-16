import { useState, useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { Button } from '../ui/Button';
import { Tooltip } from '../ui/Tooltip';
import { Input } from '../ui/Input';
import { Modal } from '../ui/Modal';
import { ConfirmDialog } from '../ui/ConfirmDialog';
import { Slider } from '../ui/Slider';
import { Toggle } from '../ui/Toggle';
import { addToast } from '../ui/Toast';
import { CustomSelect, type SelectOption } from '../ui/CustomSelect';
import {
  MAX_MEMORY_MB,
  MIN_MEMORY_MB,
  MEMORY_STEP_MB,
  getEffectiveMemoryLimitMb,
  getRecommendedMemoryMb,
} from '../../lib/memory';
import { useSettingsStore } from '../../stores/settingsStore';
import { useBrowserGuardStore } from '../../stores/browserGuardStore';
import { gcOverrideValue, memoryOverrideValue, type GcChoice } from '../../lib/instanceDefaults';
import { LoaderVersionModal } from './LoaderVersionModal';
import { useT, getLanguage } from '../../lib/i18n';

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
  /** Called with the (possibly renamed) instance name after a successful save. */
  onSaved: (name: string) => void;
}

type GcPreset = 'standard' | 'g1gc' | 'zgc';

export function InstanceEditor({ open, instance, onClose, onSaved }: Props) {
  const t = useT();
  const gb = getLanguage() === 'ru' ? 'ГБ' : 'GB';
  const presetOptions: SelectOption<GcChoice>[] = [
    {
      value: 'inherit',
      label: t('instance_editor.gc_use_default'),
      description: t('instance_editor.gc_use_default_short'),
    },
    { value: 'standard', label: t('instance_editor.gc_standard'), description: t('instance_editor.gc_standard_short') },
    { value: 'g1gc', label: t('instance_editor.gc_g1gc'), description: t('instance_editor.gc_g1gc_short') },
    { value: 'zgc', label: t('instance_editor.gc_zgc'), description: t('instance_editor.gc_zgc_req') },
  ];
  const globalConfig = useSettingsStore((s) => s.config);
  const loadGlobalConfig = useSettingsStore((s) => s.loadConfig);
  const globalDefaultMemoryMb = globalConfig?.default_memory_mb ?? null;
  const globalDefaultGcPreset = (globalConfig?.default_gc_preset as GcPreset) ?? 'g1gc';
  const globalDefaultJvmArgs = globalConfig?.default_jvm_args ?? [];
  const defaultJvmArgsText = globalDefaultJvmArgs.join(' ');

  // The instance editor can be opened before the Settings page ever loaded
  // the config. Fetch it so the "global default" labels show real values.
  useEffect(() => {
    if (open && !globalConfig) void loadGlobalConfig();
  }, [open, globalConfig, loadGlobalConfig]);

  const [name, setName] = useState(instance.name);
  const [notes, setNotes] = useState(instance.notes || '');
  const [memoryMb, setMemoryMb] = useState<number>(
    instance.memory_mb ?? globalDefaultMemoryMb ?? 4096
  );
  const [useDefaultMemory, setUseDefaultMemory] = useState(instance.memory_mb == null);
  // No per-instance override means the launcher uses its global defaults:
  // show those exact args instead of an empty field, so the value the game
  // will actually receive is visible before editing.
  const [jvmArgs, setJvmArgs] = useState(
    instance.jvm_args != null ? instance.jvm_args.join(' ') : defaultJvmArgsText
  );
  const [gcPreset, setGcPreset] = useState<GcChoice>(
    (instance.gc_preset as GcPreset) ?? 'inherit'
  );
  const [javaPath, setJavaPath] = useState(instance.java_path || '');
  const [resWidth, setResWidth] = useState(instance.resolution?.width?.toString() || '');
  const [resHeight, setResHeight] = useState(instance.resolution?.height?.toString() || '');
  const [systemRamMb, setSystemRamMb] = useState<number | null>(null);
  const [baseline, setBaseline] = useState('');
  const [confirmClose, setConfirmClose] = useState(false);
  const [loaderModalOpen, setLoaderModalOpen] = useState(false);
  const baselineMemoryRef = useRef<{ memoryMb: number; useDefaultMemory: boolean } | null>(null);

  // Values that arrive asynchronously (the global config) are read through
  // this ref by the one-time initializer below. They must not be effect
  // dependencies: when the config finished loading a second later, the effect
  // used to re-run and wipe everything the user had already typed.
  const defaultsRef = useRef({ globalDefaultMemoryMb, globalDefaultGcPreset, globalDefaultJvmArgs });
  defaultsRef.current = { globalDefaultMemoryMb, globalDefaultGcPreset, globalDefaultJvmArgs };
  const initializedForRef = useRef<string | null>(null);

  useEffect(() => {
    if (!open) return;
    // Initialize once per opened instance (the modal is reopened later with
    // the same prop object, and store reloads may produce a new identity).
    if (initializedForRef.current === instance.name) return;
    initializedForRef.current = instance.name;

    const {
      globalDefaultMemoryMb: globalMemory,
      globalDefaultJvmArgs: globalJvm,
    } = defaultsRef.current;
    // Keep the stored value as-is (within the absolute hard bounds). The
    // effective limit is shown separately and Save is blocked if the value is
    // above it; detection finishing later must never silently rewrite a saved
    // instance setting.
    const initialMemory = instance.memory_mb ?? globalMemory ?? getRecommendedMemoryMb(0);
    const resolvedMemory = Math.min(
      MAX_MEMORY_MB,
      Math.max(MIN_MEMORY_MB, initialMemory),
    );
    setName(instance.name);
    setNotes(instance.notes || '');
    setMemoryMb(resolvedMemory);
    setUseDefaultMemory(instance.memory_mb == null);
    baselineMemoryRef.current = {
      memoryMb: resolvedMemory,
      useDefaultMemory: instance.memory_mb == null,
    };
    const initialJvmArgs = instance.jvm_args != null
      ? instance.jvm_args.join(' ')
      : globalJvm.join(' ');
    setJvmArgs(initialJvmArgs);
    setGcPreset((instance.gc_preset as GcPreset) ?? 'inherit');
    setJavaPath(instance.java_path || '');
    setResWidth(instance.resolution?.width?.toString() ?? '');
    setResHeight(instance.resolution?.height?.toString() ?? '');
    // Snapshot of the just-loaded form: "dirty" means the user changed
    // something relative to this snapshot (same pattern as global Settings).
    setBaseline(JSON.stringify({
      name: instance.name,
      notes: instance.notes || '',
      memoryMb: resolvedMemory,
      useDefaultMemory: instance.memory_mb == null,
      jvmArgs: initialJvmArgs,
      gcPreset: (instance.gc_preset as GcPreset) ?? 'inherit',
      javaPath: instance.java_path || '',
      resWidth: instance.resolution?.width?.toString() ?? '',
      resHeight: instance.resolution?.height?.toString() ?? '',
    }));
  }, [open, instance]);

  // A closed editor must re-initialize the next time it is opened.
  useEffect(() => {
    if (!open) {
      initializedForRef.current = null;
      baselineMemoryRef.current = null;
    }
  }, [open]);

  const dirty =
    baseline !== '' &&
    JSON.stringify({ name, notes, memoryMb, useDefaultMemory, jvmArgs, gcPreset, javaPath, resWidth, resHeight }) !== baseline;

  // Tell the global window-close guard about unsaved edits. The editor closes
  // itself on save, so leaving the page or the whole app must ask first.
  useEffect(() => {
    if (!open) return;
    useBrowserGuardStore.getState().setUnsaved(dirty);
    return () => {
      useBrowserGuardStore.getState().setUnsaved(false);
    };
  }, [open, dirty]);

  // Editing another instance in the same mounted modal must also be guarded
  // by the NEW form's dirty state, not the previous instance's.

  /** Close via overlay click / Escape / Cancel — but ask first when dirty.
   *  While the loader version dialog is open, Escape belongs to it: both
   *  modals listen on the window, and without this guard one Escape would
   *  close the editor underneath as well. */
  const requestClose = () => {
    if (loaderModalOpen) return;
    if (dirty) setConfirmClose(true);
    else onClose();
  };

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    (async () => {
      try {
        const total = await invoke<number>('cmd_detect_system_ram');
        if (!cancelled && total > 0) setSystemRamMb(total);
      } catch {
        /* leave RAM unknown; never invent a size */
      }
    })();
    return () => { cancelled = true; };
  }, [open]);

  // Effective max memory: the safe RAM ceiling, also capped by the global
  // Settings limit. Unknown RAM must not be replaced with a fake 8 GB value;
  // a known system below 4 GB gets the hard floor and a warning.
  const configMaxMemoryMb = globalConfig?.max_memory_mb ?? null;
  const {
    maxMb: maxMemoryMb,
    ramInsufficient,
  } = getEffectiveMemoryLimitMb(configMaxMemoryMb, systemRamMb);
  const memoryAboveLimit = memoryMb > maxMemoryMb;
  const effectiveMemoryMb = Math.min(memoryMb, maxMemoryMb);

  const handleSave = async () => {
    const baselineMemory = baselineMemoryRef.current;
    const memoryChanged =
      !baselineMemory ||
      baselineMemory.memoryMb !== memoryMb ||
      baselineMemory.useDefaultMemory !== useDefaultMemory;
    if (memoryChanged && memoryAboveLimit) {
      addToast(
        t('instance_editor.memory_above_limit', {
          selected: (memoryMb / 1024).toFixed(1),
          max: (maxMemoryMb / 1024).toFixed(1),
        }),
        'error',
      );
      return;
    }
    try {
      // The field shows the global defaults when this instance has no
      // override. Saving that unchanged value must keep it as "inherit"
      // (null), otherwise the instance would pin a stale copy of the
      // defaults. Any other value — including an intentionally empty one —
      // becomes a real per-instance override.
      const normalizedJvmArgs = jvmArgs.trim();
      const normalizedDefaults = defaultJvmArgsText.trim();
      const jvmArgsOverride = normalizedJvmArgs === normalizedDefaults
        ? null
        : normalizedJvmArgs
          ? normalizedJvmArgs.split(/\s+/).filter(Boolean)
          : [];
      const updated = {
        ...instance,
        name,
        notes,
        memory_mb: memoryOverrideValue(useDefaultMemory, memoryMb),
        jvm_args: jvmArgsOverride,
        gc_preset: gcOverrideValue(gcPreset),
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
      await invoke('cmd_save_instance', { instance: updated, oldName: instance.name });
      addToast(t('instance_editor.saved_toast'), 'success');
      onSaved(updated.name);
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

  const systemRamGb = systemRamMb != null ? (systemRamMb / 1024).toFixed(1) : null;
  const allocatedGb = (memoryMb / 1024).toFixed(1);
  const maxGb = (maxMemoryMb / 1024).toFixed(1);
  const zgcDisabled = effectiveMemoryMb < 8192;

  return (
    <>
    <Modal open={open} onClose={requestClose} title={t('instance_editor.title')} fitContent>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
        <Input label={t('instance_editor.name_label')} id="edit-name" type="text" value={name} onChange={(e) => setName(e.target.value)} />
        <Input label={t('instance_editor.notes_label')} id="edit-notes" type="text" value={notes} onChange={(e) => setNotes(e.target.value)} />

        {/* Mod loader version (hidden for vanilla instances) — the picker
            installs the chosen version and updates the instance. Styled
            exactly like the Java path row (field + button) so it reads as
            a real setting instead of a stray line of text (owner: «выглядит
            как какая-то надпись и ты его даже не заметишь»). */}
        {instance.loader && instance.loader !== 'Vanilla' && (
          <div>
            <label className="input__label" style={{ display: 'block', marginBottom: 4 }}>
              {t('instance_editor.loader_label')}
            </label>
            <div style={{ display: 'flex', gap: 'var(--space-sm)' }}>
              {/* A read-only form field looked editable, so the value now
                  reads as a filled chip: name + version, no input chrome. */}
              <div
                title={`${instance.loader}${instance.loader_version ? ` ${instance.loader_version}` : ''}`}
                style={{
                  flex: 1,
                  minWidth: 0,
                  display: 'flex',
                  alignItems: 'baseline',
                  gap: 'var(--space-sm)',
                  padding: 'var(--space-md) var(--space-lg)',
                  background: 'var(--surface-glass)',
                  borderRadius: 'var(--radius-md)',
                }}
              >
                <span style={{ fontWeight: 600, flexShrink: 0 }}>{instance.loader}</span>
                {instance.loader_version && (
                  <span
                    style={{
                      color: 'var(--text-tertiary)',
                      fontSize: 'var(--font-size-sm)',
                      minWidth: 0,
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                    }}
                  >
                    {instance.loader_version}
                  </span>
                )}
              </div>
              <Button size="sm" variant="ghost" onClick={() => setLoaderModalOpen(true)}>
                {t('instance_editor.loader_change')}
              </Button>
            </div>
          </div>
        )}

        {/* Memory slider with dynamic max */}
        <div>
          <label className="input__label" style={{ display: 'block', marginBottom: 6 }}>
            {t('instance_editor.memory_label')} <strong>{allocatedGb} {gb}</strong>
            <span style={{ color: 'var(--text-tertiary)', marginLeft: 8, fontSize: 'var(--font-size-xs)' }}>
              {systemRamGb != null
                ? t('instance_editor.memory_max_info', { maxGb, systemRamGb })
                : t('instance_editor.memory_unknown_ram_short')}
            </span>
          </label>
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', marginBottom: 6 }}>
            <Toggle
              checked={useDefaultMemory}
              onChange={(value) => {
                setUseDefaultMemory(value);
                if (value && globalDefaultMemoryMb) setMemoryMb(globalDefaultMemoryMb);
              }}
              ariaLabel={t('instance_editor.memory_use_default', {
                gb: ((globalDefaultMemoryMb ?? 4096) / 1024).toFixed(1),
              })}
            />
            <span style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
              {t('instance_editor.memory_use_default', {
                gb: ((globalDefaultMemoryMb ?? 4096) / 1024).toFixed(1),
              })}
            </span>
          </div>
          <div style={{ opacity: useDefaultMemory ? 0.55 : 1 }}>
            <Slider
              min={MIN_MEMORY_MB}
              max={maxMemoryMb}
              step={MEMORY_STEP_MB}
              value={memoryMb}
              onChange={(v) => setMemoryMb(v)}
              disabled={useDefaultMemory}
            />
          </div>
          <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: 2 }}>
            <span>{t('instance_editor.memory_min_label', { gb: (MIN_MEMORY_MB / 1024).toFixed(1) })}</span>
            <span>{t('instance_editor.memory_max_label', { maxGb })}</span>
          </div>
          {memoryAboveLimit && (
            <div
              style={{
                marginTop: 6,
                fontSize: 'var(--font-size-xs)',
                color: 'var(--color-warning)',
                lineHeight: 1.45,
              }}
            >
              {t('instance_editor.memory_above_limit', {
                selected: allocatedGb,
                max: maxGb,
              })}
            </div>
          )}
          {systemRamMb == null && (
            <div
              style={{
                marginTop: 6,
                fontSize: 'var(--font-size-xs)',
                color: 'var(--text-tertiary)',
                lineHeight: 1.45,
              }}
            >
              {t('instance_editor.memory_unknown_ram')}
            </div>
          )}
          {ramInsufficient && (
            <div
              style={{
                marginTop: 6,
                fontSize: 'var(--font-size-xs)',
                color: 'var(--color-warning)',
                lineHeight: 1.45,
              }}
            >
              {t('instance_editor.memory_low_ram')}
            </div>
          )}
          {globalDefaultMemoryMb && globalDefaultMemoryMb !== memoryMb && (
            <div style={{ marginTop: 6 }}>
              <Tooltip content={t('instance_editor.gc_default_tooltip', { gb: (globalDefaultMemoryMb / 1024).toFixed(1) })}>
                <button
                  type="button"
                  className="btn btn--ghost btn--sm"
                  onClick={() => setMemoryMb(globalDefaultMemoryMb)}
                >
                  {t('instance_editor.memory_reset_to_global')}
                </button>
              </Tooltip>
            </div>
          )}
        </div>

        {/* GC preset dropdown */}
        <div>
          <label className="input__label" style={{ display: 'block', marginBottom: 4 }}>
            {t('instance_editor.gc_label')}
          </label>
          <CustomSelect<GcChoice>
            value={gcPreset}
            options={presetOptions.map((o) =>
              o.value === 'zgc' && zgcDisabled ? { ...o, disabled: true } : o
            )}
            onChange={setGcPreset}
          />
          <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: 4 }}>
            {gcPreset === 'inherit' && t('instance_editor.gc_default_desc', { preset: globalDefaultGcPreset.toUpperCase() })}
            {gcPreset === 'standard' && t('instance_editor.gc_standard_desc')}
            {gcPreset === 'g1gc' && t('instance_editor.gc_g1gc_desc')}
            {gcPreset === 'zgc' && (
              zgcDisabled
                ? <span style={{ color: 'var(--color-warning)' }}>{t('instance_editor.gc_zgc_unavailable', { allocatedGb })}</span>
                : <>{t('instance_editor.gc_zgc_desc')} <span style={{ color: 'var(--color-warning)' }}>{t('instance_editor.gc_zgc_warning')}</span></>
            )}
            {gcPreset === 'inherit' && globalDefaultGcPreset === 'zgc' && zgcDisabled && (
              <div style={{ color: 'var(--color-warning)', marginTop: 4 }}>
                {t('instance_editor.gc_zgc_unavailable', { allocatedGb })}
              </div>
            )}
          </div>
          {(gcPreset === 'zgc' ||
            (gcPreset === 'inherit' && globalDefaultGcPreset === 'zgc')) && (
            <div
              style={{
                fontSize: 'var(--font-size-xs)',
                color: 'var(--text-tertiary)',
                marginTop: 4,
                lineHeight: 1.45,
              }}
            >
              {t('instance_editor.gc_zgc_launch_note')}
            </div>
          )}
        </div>

        <Input
          label={t('instance_editor.jvm_label')}
          id="edit-jvm"
          type="text"
          value={jvmArgs}
          onChange={(e) => setJvmArgs(e.target.value)}
        />
        <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: -4 }}>
          {jvmArgs.trim() === defaultJvmArgsText.trim()
            ? t('instance_editor.jvm_default_hint')
            : t('instance_editor.jvm_custom_hint')}
        </div>
        {defaultJvmArgsText && jvmArgs.trim() !== defaultJvmArgsText.trim() && (
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => setJvmArgs(defaultJvmArgsText)}
            style={{ alignSelf: 'flex-start' }}
          >
            {t('instance_editor.jvm_reset_default')}
          </button>
        )}

        <div>
          <label className="input__label" style={{ display: 'block', marginBottom: 4 }}>{t('instance_editor.java_path_label')}</label>
          <div style={{ display: 'flex', gap: 'var(--space-xs)' }}>
            <input className="input" type="text" value={javaPath} onChange={(e) => setJavaPath(e.target.value)} style={{ flex: 1 }} />
            <Button size="sm" variant="ghost" onClick={pickJavaPath}>{t('instance_editor.browse_btn')}</Button>
          </div>
        </div>
        <div>
          <label className="input__label" style={{ display: 'block', marginBottom: 4 }}>{t('instance_editor.resolution_label')}</label>
          <div style={{ display: 'flex', gap: 'var(--space-xs)', alignItems: 'center' }}>
            <input className="input" type="number" value={resWidth} onChange={(e) => setResWidth(e.target.value)} style={{ flex: 1 }} />
            <span style={{ color: 'var(--text-tertiary)' }}>×</span>
            <input className="input" type="number" value={resHeight} onChange={(e) => setResHeight(e.target.value)} style={{ flex: 1 }} />
          </div>
        </div>
      </div>
      {/* Floating save button (bottom-right), only while there are unsaved
          changes — same pattern as the global Settings page. Portaled above
          the modal overlay (z 1000). */}
      {open && dirty && createPortal(
        <div className="settings-save-fab" style={{ zIndex: 1100 }}>
          <button className="btn btn--primary" onClick={handleSave}>{t('common.save')}</button>
        </div>,
        document.body
      )}
      {/* Unsaved-changes confirmation when closing via overlay / Escape.
          Shared confirm modal, portaled to the body like the FAB so it is
          never clipped by the editor sheet's own scroll container. */}
      {createPortal(
        <ConfirmDialog
          open={confirmClose}
          title={t('instance_editor.close_confirm_title')}
          description={t('instance_editor.close_confirm_text')}
          onConfirm={() => {
            setConfirmClose(false);
            onClose();
          }}
          onCancel={() => setConfirmClose(false)}
          confirmLabel={t('instance_editor.close_discard')}
          cancelLabel={t('instance_editor.close_stay')}
        />,
        document.body
      )}
    </Modal>
    {instance.loader && instance.loader !== 'Vanilla' && (
      <LoaderVersionModal
        open={loaderModalOpen}
        instanceName={instance.name}
        loader={instance.loader}
        mcVersion={instance.mc_version}
        currentVersion={instance.loader_version ?? null}
        onClose={() => setLoaderModalOpen(false)}
        onUpdated={() => {
          setLoaderModalOpen(false);
          onSaved(instance.name);
          onClose();
        }}
      />
    )}
    </>
  );
}
