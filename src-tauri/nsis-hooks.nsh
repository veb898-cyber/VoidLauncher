; NSIS hooks bundled into the generated installer (bundle.windows.nsis.installerHooks).
;
; Persist the language chosen in the installer's language dialog. Tauri's stock
; template shows the selector from .onInit (MUI_LANGDLL_DISPLAY) and even clears
; the value when the app is uninstalled — but it never writes it. Without a
; stored value every install asks again, and the launcher has nothing to boot
; with: cmd_get_installer_language reads this exact key/value on first start.
; $LANGUAGE is the selected LANGID ("1033" = English, "1049" = Russian) and is
; still set when the install section runs, so the hook can store it as-is —
; MUI_LANGDLL_DISPLAY then skips the dialog on the next (re)install.
!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "${MANUPRODUCTKEY}" "Installer Language" $LANGUAGE
!macroend
