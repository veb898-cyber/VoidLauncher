/// Simple backend i18n for user-facing messages.
/// The frontend passes `lang: "en" | "ru"` to Tauri commands.
pub fn tr(lang: &str, key: &str, args: &[(&str, &str)]) -> String {
    let msg = match (lang, key) {
        ("ru", "installing_loader") =>
            "Установка {loader} {version} для {mc}...",
        ("ru", "downloading_loader_libs") =>
            "Загрузка библиотек {loader}...",
        ("ru", "loader_install_failed") =>
            "Не удалось установить {loader}: {error}",
        ("ru", "loader_install_success") =>
            "{loader} успешно установлен",
        ("ru", "instance_no_loader") =>
            "У сборки нет загрузчика модов",
        ("ru", "instance_no_loader_version") =>
            "У сборки не указана версия загрузчика",
        ("ru", "loader_version_mismatch") =>
            "Загрузчик {loader} предназначен для Minecraft {loader_version}, а сборка '{instance_name}' использует {instance_version}",
        _ => match key {
            "installing_loader" =>
                "Installing {loader} {version} for {mc}...",
            "downloading_loader_libs" =>
                "Downloading {loader} libraries...",
            "loader_install_failed" =>
                "Failed to install {loader}: {error}",
            "loader_install_success" =>
                "{loader} installed successfully",
            "instance_no_loader" =>
                "Instance has no mod loader",
            "instance_no_loader_version" =>
                "Instance has no loader version set",
            "loader_version_mismatch" =>
                "Loader {loader} targets Minecraft {loader_version}, but instance '{instance_name}' uses {instance_version}",
            _ => key,
        },
    };

    let mut s = msg.to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{}}}", k), v);
    }
    s
}
