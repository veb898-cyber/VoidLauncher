import {
  forwardRef,
  useCallback,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
} from 'react';
import { useT } from '../../lib/i18n';
import {
  MOD_LIST_VARIABLE_TOKENS,
  type ModListVariable,
} from '../../lib/modListExport';
import { insertTemplateVariable } from '../../lib/modListExportFile';
import { MAX_CUSTOM_TEMPLATE_LENGTH } from '../../stores/modListExportStore';

export interface ModListTemplateEditorHandle {
  insertVariable: (variable: ModListVariable) => void;
}

interface ModListTemplateEditorProps {
  value: string;
  onChange: (value: string) => void;
}

/**
 * Compact template textarea. Variable buttons live next to it in the dialog's
 * options column and insert through the imperative handle, so the caret and
 * any selected text keep the normal textarea behaviour.
 */
export const ModListTemplateEditor = forwardRef<
  ModListTemplateEditorHandle,
  ModListTemplateEditorProps
>(function ModListTemplateEditor({ value, onChange }, ref) {
  const t = useT();
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const pendingCursor = useRef<number | null>(null);

  const insertVariable = useCallback(
    (variable: ModListVariable) => {
      const element = textareaRef.current;
      const start = element?.selectionStart ?? value.length;
      const end = element?.selectionEnd ?? value.length;
      const next = insertTemplateVariable(
        value,
        start,
        end,
        MOD_LIST_VARIABLE_TOKENS[variable],
      );
      pendingCursor.current = next.cursor;
      onChange(next.value);
    },
    [onChange, value],
  );

  useImperativeHandle(
    ref,
    () => ({ insertVariable }),
    [insertVariable],
  );

  useLayoutEffect(() => {
    const element = textareaRef.current;
    const cursor = pendingCursor.current;
    if (!element || cursor === null) return;
    pendingCursor.current = null;
    element.focus();
    element.setSelectionRange(cursor, cursor);
  }, [value]);

  return (
    <textarea
      ref={textareaRef}
      className="input mod-export__template"
      value={value}
      maxLength={MAX_CUSTOM_TEMPLATE_LENGTH}
      spellCheck={false}
      aria-label={t('mod_export.template_label')}
      onChange={(event) => onChange(event.target.value)}
    />
  );
});
