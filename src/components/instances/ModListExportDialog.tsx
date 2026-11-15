import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AlertTriangle, Check, Copy, Package } from 'lucide-react';
import { Button } from '../ui/Button';
import { CustomSelect } from '../ui/CustomSelect';
import { Modal } from '../ui/Modal';
import { SnakeSpinner } from '../ui/SnakeSpinner';
import { addToast } from '../ui/Toast';
import { useT, type MessageKey } from '../../lib/i18n';
import {
  MOD_LIST_FORMATS,
  MOD_LIST_VARIABLES,
  MOD_LIST_VARIABLE_TOKENS,
  buildModListEntries,
  exportExtension,
  generateModList,
  previewKind,
  templateEscapeForExtension,
  templateForFormat,
  type CustomTemplateEscape,
  type ModListFormat,
  type ModListExportOptions,
  type ModListSourceItem,
} from '../../lib/modListExport';
import {
  copyModListText,
  saveModListExport,
} from '../../lib/modListExportFile';
import { useModListExportStore } from '../../stores/modListExportStore';
import { ModListPreview } from './ModListPreview';
import {
  ModListTemplateEditor,
  type ModListTemplateEditorHandle,
} from './ModListTemplateEditor';

const FORMAT_LABELS: Record<ModListFormat, MessageKey> = {
  html: 'mod_export.format_html',
  markdown: 'mod_export.format_markdown',
  text: 'mod_export.format_text',
  json: 'mod_export.format_json',
  csv: 'mod_export.format_csv',
  custom: 'mod_export.format_custom',
};

const FIELD_CONFIG: Array<{
  key: keyof ModListExportOptions;
  label: MessageKey;
}> = [
  { key: 'version', label: 'mod_export.field_version' },
  { key: 'authors', label: 'mod_export.field_authors' },
  { key: 'url', label: 'mod_export.field_url' },
  { key: 'filename', label: 'mod_export.field_filename' },
  { key: 'enabled', label: 'mod_export.field_enabled' },
];

const EXTENSION_PRESETS = ['txt', 'md', 'html', 'json', 'csv'] as const;

const ESCAPE_HINTS: Record<CustomTemplateEscape, MessageKey> = {
  html: 'mod_export.escape_html',
  markdown: 'mod_export.escape_markdown',
  json: 'mod_export.escape_json',
  csv: 'mod_export.escape_csv',
  none: 'mod_export.escape_none',
};

interface ModListExportDialogProps {
  open: boolean;
  onClose: () => void;
  instanceName: string;
  items: readonly ModListSourceItem[];
  /** The mods tab is still reading the folder (no entries yet). */
  loading?: boolean;
}

export function ModListExportDialog({
  open,
  onClose,
  instanceName,
  items,
  loading = false,
}: ModListExportDialogProps) {
  const t = useT();
  const format = useModListExportStore((state) => state.format);
  const options = useModListExportStore((state) => state.options);
  const customTemplate = useModListExportStore((state) => state.customTemplate);
  const customExtension = useModListExportStore((state) => state.customExtension);
  const setFormat = useModListExportStore((state) => state.setFormat);
  const setOption = useModListExportStore((state) => state.setOption);
  const setCustomTemplate = useModListExportStore((state) => state.setCustomTemplate);
  const setCustomExtension = useModListExportStore((state) => state.setCustomExtension);

  const [saving, setSaving] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const copyTimer = useRef<number | null>(null);
  const templateRef = useRef<ModListTemplateEditorHandle | null>(null);

  // The export always covers the complete installed list, never the page
  // selection or the current search results. Disabled mods stay included.
  const entries = useMemo(() => buildModListEntries(items), [items]);
  // The dialog component stays mounted while closed; generating the full
  // preview (a 375-entry document for a large instance) on every list change
  // was wasted work. Only build it while the dialog is actually open.
  const content = useMemo(
    () =>
      open
        ? generateModList(entries, {
            format,
            options,
            customTemplate,
            customExtension,
            instanceName,
          })
        : '',
    [open, entries, format, options, customTemplate, customExtension, instanceName],
  );

  const extension = exportExtension(format, customExtension);
  const extensionValid =
    format !== 'custom' || /^[a-z0-9]{1,10}$/.test(customExtension);
  const templateValid =
    format !== 'custom' || customTemplate.trim().length > 0;
  const canExport = entries.length > 0 && extensionValid && templateValid;
  const renderedKind = previewKind(format, customExtension);
  const templateValue = templateForFormat(format, customTemplate);
  const escapeHint = ESCAPE_HINTS[templateEscapeForExtension(customExtension)];

  const formatOptions = MOD_LIST_FORMATS.map((value) => ({
    value,
    label: t(FORMAT_LABELS[value]),
  }));

  useEffect(() => {
    if (open) {
      setCopied(false);
      setError(null);
      return;
    }
    setSaving(false);
    setCopied(false);
    setError(null);
  }, [open]);

  useEffect(() => {
    setCopied(false);
    setError(null);
    if (copyTimer.current !== null) {
      window.clearTimeout(copyTimer.current);
      copyTimer.current = null;
    }
  }, [content]);

  useEffect(
    () => () => {
      if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    },
    [],
  );

  const handleTemplateChange = (value: string) => {
    setCustomTemplate(value);
    // Prism turns the format into Custom when the template is edited. Keeping
    // the same behaviour means the user never edits an example that is then
    // ignored by the selected built-in format. Carry the matching extension
    // over so the Custom escaping/file type follows the example they edited.
    if (format !== 'custom') {
      setCustomExtension(exportExtension(format, customExtension));
      setFormat('custom');
    }
  };

  const handleCopy = async () => {
    if (!canExport) return;
    setError(null);
    try {
      const { writeText } = await import(
        '@tauri-apps/plugin-clipboard-manager'
      );
      await copyModListText(content, { writeText });
      setCopied(true);
      addToast(t('mod_export.copy_success'), 'success');
      if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(() => {
        copyTimer.current = null;
        setCopied(false);
      }, 1600);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setError(t('mod_export.copy_error', { error: message }));
      addToast(t('mod_export.copy_error', { error: message }), 'error');
    }
  };

  const handleSave = async () => {
    if (!canExport || saving) return;
    setSaving(true);
    setError(null);
    try {
      const result = await saveModListExport(
        { content, instanceName, format, customExtension },
        {
          choosePath: async (defaultPath, selectedExtension) => {
            const { save } = await import('@tauri-apps/plugin-dialog');
            return save({
              title: t('mod_export.title'),
              defaultPath,
              filters: [
                {
                  name: t('mod_export.file_filter'),
                  extensions: [selectedExtension],
                },
              ],
            });
          },
          writeFile: async (path, text, addUtf8Bom) => {
            await invoke('cmd_save_export_file', {
              path,
              content: text,
              addUtf8Bom,
            });
          },
        },
      );
      if (result.status === 'saved') {
        const name = result.path.split(/[\\/]/).pop() || result.path;
        addToast(t('mod_export.save_success', { name }), 'success');
      }
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setError(t('mod_export.save_error', { error: message }));
      addToast(t('mod_export.save_error', { error: message }), 'error');
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t('mod_export.title')}
      maxWidth={660}
      dense
      footer={
        <div className="mod-export__footer">
          <Button
            variant="secondary"
            onClick={handleCopy}
            disabled={!canExport || saving}
          >
            {copied ? (
              <Check size={14} aria-hidden />
            ) : (
              <Copy size={14} aria-hidden />
            )}
            {copied ? t('mod_export.copied') : t('mod_export.copy')}
          </Button>
          <div className="mod-export__footer-right">
            <Button onClick={handleSave} loading={saving} disabled={!canExport}>
              {saving ? t('mod_export.saving') : t('common.save')}
            </Button>
            <Button variant="ghost" onClick={onClose}>
              {t('common.cancel')}
            </Button>
          </div>
        </div>
      }
    >
      <div className="mod-export">
        <div className="mod-export__format-row">
          <div className="mod-export__format-field">
            <label className="mod-export__label" htmlFor="mod-export-format">
              {t('mod_export.format_label')}
            </label>
            <CustomSelect<ModListFormat>
              id="mod-export-format"
              className="mod-export__format"
              value={format}
              options={formatOptions}
              onChange={setFormat}
            />
          </div>

          {format === 'custom' && (
            <div className="mod-export__extension-field">
              <label
                className="mod-export__label"
                htmlFor="mod-export-extension"
              >
                {t('mod_export.extension_label')}
              </label>
              <div className="mod-export__ext-compact">
                <div className="mod-export__ext">
                  <span aria-hidden>.</span>
                  <input
                    id="mod-export-extension"
                    className="input"
                    value={customExtension}
                    maxLength={10}
                    spellCheck={false}
                    aria-invalid={!extensionValid}
                    aria-label={t('mod_export.extension_label')}
                    onChange={(event) =>
                      setCustomExtension(event.target.value)
                    }
                  />
                </div>
                <div className="mod-export__ext-presets">
                  {EXTENSION_PRESETS.map((preset) => (
                    <button
                      key={preset}
                      type="button"
                      className={`mod-export__ext-preset${
                        customExtension === preset
                          ? ' mod-export__ext-preset--active'
                          : ''
                      }`}
                      aria-pressed={customExtension === preset}
                      onClick={() => setCustomExtension(preset)}
                    >
                      .{preset}
                    </button>
                  ))}
                </div>
              </div>
              <div
                className={`mod-export__hint${
                  extensionValid ? '' : ' mod-export__hint--error'
                }`}
              >
                {extensionValid
                  ? t(escapeHint)
                  : t('mod_export.extension_invalid')}
              </div>
            </div>
          )}
        </div>

        <div className="mod-export__options-row">
          <div className="mod-export__column">
            <div className="mod-export__label">
              {t('mod_export.template_label')}
            </div>
            <ModListTemplateEditor
              ref={templateRef}
              value={templateValue}
              onChange={handleTemplateChange}
            />
            <div className="mod-export__hint">
              {format === 'custom'
                ? t('mod_export.template_hint')
                : t('mod_export.template_edit_hint')}
            </div>
          </div>

          <div className="mod-export__column">
            {format === 'custom' ? (
              <>
                <div className="mod-export__label">
                  {t('mod_export.variables_label')}
                </div>
                <div className="mod-export__vars">
                  {MOD_LIST_VARIABLES.map((variable) => (
                    <button
                      key={variable}
                      type="button"
                      className="mod-export__var"
                      title={t(`mod_export.var_${variable}` as MessageKey)}
                      aria-label={`${MOD_LIST_VARIABLE_TOKENS[variable]} — ${t(
                        `mod_export.var_${variable}` as MessageKey,
                      )}`}
                      onClick={() => templateRef.current?.insertVariable(variable)}
                    >
                      {MOD_LIST_VARIABLE_TOKENS[variable]}
                    </button>
                  ))}
                </div>
              </>
            ) : (
              <>
                <div className="mod-export__label">
                  {t('mod_export.fields_label')}
                </div>
                <div className="mod-export__checks">
                  {FIELD_CONFIG.map(({ key, label }) => (
                    <label key={key} className="mod-export__check">
                      <input
                        type="checkbox"
                        checked={options[key]}
                        onChange={(event) =>
                          setOption(key, event.target.checked)
                        }
                      />
                      <span>{t(label)}</span>
                    </label>
                  ))}
                </div>
              </>
            )}
          </div>
        </div>

        <div className="mod-export__section">
          <div className="mod-export__section-head">
            <span className="mod-export__label">
              {t('mod_export.preview_label')}
              {format !== 'custom' || extensionValid ? (
                <span className="mod-export__extension-badge">.{extension}</span>
              ) : null}
            </span>
          </div>
          {loading && entries.length === 0 ? (
            <div className="mod-export__empty">
              <SnakeSpinner size={18} />
              <div>{t('mod_export.loading')}</div>
            </div>
          ) : entries.length === 0 ? (
            <div className="mod-export__empty">
              <Package size={24} aria-hidden />
              <div>{t('mod_export.empty')}</div>
              <div className="mod-export__hint">
                {t('mod_export.empty_hint')}
              </div>
            </div>
          ) : !templateValid ? (
            <div className="mod-export__empty">
              <Package size={24} aria-hidden />
              <div>{t('mod_export.template_required')}</div>
            </div>
          ) : (
            <ModListPreview content={content} kind={renderedKind} />
          )}
        </div>

        {error && (
          <div className="mod-export__error" role="alert">
            <AlertTriangle size={14} aria-hidden />
            <span>{error}</span>
          </div>
        )}
      </div>
    </Modal>
  );
}
