import { useT, type Language } from '../../lib/i18n';
import { useLanguageStore } from '../../stores/languageStore';
import { useThemeStore, type Theme } from '../../stores/themeStore';
import { useWallpaperStore } from '../../stores/wallpaperStore';
import { useContentViewStore } from '../../stores/contentViewStore';
import { useFontStore, type Font, type ConsoleFont } from '../../stores/fontStore';
import type { WallpaperId } from '../../lib/wallpapers';
import { CustomSelect } from '../../components/ui/CustomSelect';
import { Toggle } from '../../components/ui/Toggle';

export function AppearanceSettings() {
  const t = useT();
  const language = useLanguageStore((s) => s.language);
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);
  const liquidGlass = useThemeStore((s) => s.liquidGlass);
  const setLiquidGlass = useThemeStore((s) => s.setLiquidGlass);
  const wallpaper = useWallpaperStore((s) => s.wallpaper);
  const setWallpaper = useWallpaperStore((s) => s.setWallpaper);
  const contentCards = useContentViewStore((s) => s.cards);
  const setContentCards = useContentViewStore((s) => s.setCards);
  const font = useFontStore((s) => s.font);
  const consoleFont = useFontStore((s) => s.consoleFont);

  // Name of the normal theme, so the Liquid Glass note can say which theme
  // will come back instead of speaking about "the theme".
  const themeLabel: Record<Theme, string> = {
    standard: t('settings.theme_standard'),
    ember: t('settings.theme_ember'),
    dark: t('settings.theme_dark'),
    phosphor: t('settings.theme_phosphor'),
    carbon: t('settings.theme_carbon'),
    blueprint: t('settings.theme_blueprint'),
  };

  return (
    <>
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_interface')}</h2>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.language_title')}</div>
            <div className="settings-row__desc">
              {t('settings.language_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 220 }}>
            <CustomSelect
              value={language}
              options={[
                { value: 'en', label: 'English' },
                { value: 'ru', label: 'Русский' },
              ]}
              onChange={(v) => useLanguageStore.getState().setLanguage(v as Language)}
            />
          </div>
        </div>
        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.font_title')}</div>
            <div className="settings-row__desc">
              {t('settings.font_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 220 }}>
            <CustomSelect
              value={font}
              options={[
                { value: 'default', label: t('settings.font_default') },
                { value: 'monocraft', label: t('settings.font_monocraft') },
              ]}
              onChange={(v) => useFontStore.getState().setFont(v as Font)}
            />
          </div>
        </div>
        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.console_font_title')}</div>
            <div className="settings-row__desc">
              {t('settings.console_font_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 220 }}>
            <CustomSelect
              value={consoleFont}
              options={[
                {
                  value: 'auto',
                  label: t('settings.console_font_auto'),
                  tooltip: t('settings.console_font_auto_hint'),
                },
                {
                  value: 'default',
                  label: t('settings.console_font_default'),
                  tooltip: t('settings.console_font_default_hint'),
                },
                {
                  value: 'monocraft',
                  label: t('settings.console_font_monocraft'),
                },
              ]}
              onChange={(v) => useFontStore.getState().setConsoleFont(v as ConsoleFont)}
            />
          </div>
        </div>
        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.theme_title')}</div>
            <div className="settings-row__desc">
              {t('settings.theme_desc')}
            </div>
            {liquidGlass && (
              <div className="settings-row__note settings-row__note--locked">
                {t('settings.theme_locked')}
              </div>
            )}
          </div>
          <div className="settings-row__control" style={{ width: 220 }}>
            <CustomSelect
              value={theme}
              disabled={liquidGlass}
              options={[
                { value: 'standard', label: t('settings.theme_standard') },
                { value: 'ember', label: t('settings.theme_ember') },
                { value: 'dark', label: t('settings.theme_dark') },
                { value: 'phosphor', label: t('settings.theme_phosphor') },
                { value: 'carbon', label: t('settings.theme_carbon') },
                { value: 'blueprint', label: t('settings.theme_blueprint') },
              ].map((o) => ({ ...o, disabled: liquidGlass }))}
              onChange={(v) => setTheme(v as Theme)}
            />
          </div>
        </div>
      </section>

      <section className="settings-section">
        <h2 className="settings-section__title">
          {t('settings.exp_section_title')}
          <span className="experimental-badge experimental-badge--inline">
            {t('common.experimental')}
          </span>
        </h2>
        <p className="settings-section__desc">{t('settings.exp_section_desc')}</p>

        {/* Liquid Glass is a separate experimental rendering mode, not a
            colour preset of the theme above: while it is on, the normal
            themes stay visible but locked, and the selected one is kept so
            it can be restored on the way back. */}
        <div className="experimental-block">
          <div className="settings-row">
            <div className="settings-row__info">
              <div className="settings-row__title">
                {t('settings.lg_title')}
                <span className="experimental-badge experimental-badge--inline">
                  {t('common.experimental')}
                </span>
              </div>
              <div className="settings-row__desc">{t('settings.lg_desc')}</div>
              <div className="settings-row__note">{t('settings.lg_perf_note')}</div>
              {liquidGlass && (
                <div className="settings-row__note settings-row__note--active">
                  {t('settings.lg_active', { theme: themeLabel[theme] })}
                </div>
              )}
            </div>
            <div className="settings-row__control">
              <Toggle checked={liquidGlass} onChange={setLiquidGlass} />
            </div>
          </div>
          {/* The backdrop only exists under Liquid Glass, so it is disabled
              rather than hidden in the other appearances: the choice is kept and
              comes back when Liquid Glass is switched on again. */}
          <div className="settings-row">
            <div className="settings-row__info">
              <div className="settings-row__title">
                {t('settings.wallpaper_title')}
                <span className="experimental-badge experimental-badge--inline">
                  {t('common.experimental')}
                </span>
              </div>
              <div className="settings-row__desc">{t('settings.wallpaper_desc')}</div>
              {!liquidGlass && (
                <div className="settings-row__note settings-row__note--locked">
                  {t('settings.wallpaper_locked')}
                </div>
              )}
            </div>
            <div className="settings-row__control" style={{ width: 220 }}>
              <CustomSelect<WallpaperId>
                value={wallpaper}
                disabled={!liquidGlass}
                options={[
                  { value: 'waves', label: t('settings.wallpaper_waves') },
                  { value: 'ribbons', label: t('settings.wallpaper_ribbons') },
                  { value: 'mesh', label: t('settings.wallpaper_mesh') },
                  { value: 'nebula', label: t('settings.wallpaper_nebula') },
                ]}
                onChange={setWallpaper}
              />
            </div>
          </div>
          {/* The card lists only exist under Liquid Glass (the normal themes
              keep the original section layouts), so the choice is disabled
              rather than hidden while Liquid Glass is off — same reading as
              the backdrop row above. Default on. */}
          <div className="settings-row">
            <div className="settings-row__info">
              <div className="settings-row__title">
                {t('settings.content_cards_title')}
                <span className="experimental-badge experimental-badge--inline">
                  {t('common.experimental')}
                </span>
              </div>
              <div className="settings-row__desc">{t('settings.content_cards_desc')}</div>
              {!liquidGlass && (
                <div className="settings-row__note settings-row__note--locked">
                  {t('settings.content_cards_locked')}
                </div>
              )}
            </div>
            <div className="settings-row__control">
              <Toggle checked={contentCards} onChange={setContentCards} disabled={!liquidGlass} />
            </div>
          </div>
        </div>
      </section>
    </>
  );
}
