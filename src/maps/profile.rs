use anyhow::{Context, Result, ensure};

pub fn folder_name(name: &str) -> String {
    name.as_bytes().iter().map(|b| format!("{b:02X}")).collect()
}

pub fn quote(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            32..=126 => out.push(byte as char),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out
}

pub fn mod_block(names: &[String]) -> Result<String> {
    let mut block = format!(" active_mods: {}\n", names.len());
    for (index, stem) in names.iter().enumerate() {
        ensure!(!stem.contains('|'), "Недопустимый идентификатор мода");
        block.push_str(&format!(
            " active_mods[{index}]: \"{}|{}\"\n",
            quote(stem),
            quote(stem)
        ));
    }
    Ok(block)
}

/// New profile metadata only. ETS2 initializes the career on its first load.
/// No save, finances, achievements, inventory or third-party profile is copied.
/// Format 6 is written by ETS2 1.61. Legacy format 0 triggers game migration
/// which enables all subscribed Workshop mods, breaking build isolation.
pub fn create(name: &str, module: &str, mods: &[String]) -> Result<Vec<u8>> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 80 && !name.chars().any(char::is_control),
        "Имя профиля: от 1 до 80 байт, без управляющих символов"
    );
    let stamp = super::source::now();
    Ok(format!("SiiNunit\n{{\nuser_profile : _nameless.0.1 {{\n face: 0\n brand: scania_r\n map_path: \"{}\"\n logo: logo_0\n company_name: \"Truck Launcher\"\n male: true\n cached_experience: 0\n cached_distance: 0\n user_data: 0\n{} customization: 0\n cached_stats: 0\n cached_discovery: 0\n profile_name: \"{}\"\n creation_time: {stamp}\n save_time: {stamp}\n version: 6\n online_user_name: \"\"\n online_password: \"\"\n}}\n}}\n", quote(module), mod_block(mods)?, quote(name)).into_bytes())
}

pub fn decode(bytes: &[u8]) -> Result<String> {
    ensure!(bytes.len() <= 8 * 1024 * 1024, "Профиль слишком большой");
    let decoded = std::panic::catch_unwind(|| decrypt_truck::decrypt_bin_file(&bytes.to_vec()))
        .map_err(|_| anyhow::anyhow!("Повреждён бинарный профиль"))?
        .map_err(|e| anyhow::anyhow!("Не удалось прочитать профиль: {e}"))?;
    String::from_utf8(decoded).context("Некорректная кодировка профиля")
}

pub fn update(bytes: &[u8], module: &str, mods: &[String]) -> Result<Vec<u8>> {
    let text = decode(bytes)?;
    let count = regex::Regex::new(r"^\s*active_mods\s*:\s*\d+\s*$")?;
    let item = regex::Regex::new(r"^\s*active_mods\s*\[\d+\]\s*:")?;
    let mut found = false;
    let mut map_found = false;
    let mut out = String::new();
    for line in text.lines() {
        if count.is_match(line) {
            ensure!(!found, "Неоднозначный список модов в профиле");
            out.push_str(&mod_block(mods)?);
            found = true;
        } else if item.is_match(line) {
            continue;
        } else if line.trim_start().starts_with("map_path:") {
            ensure!(
                line.contains(&format!("\"{}\"", quote(module))),
                "Смена модуля требует новой сборки"
            );
            map_found = true;
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    ensure!(
        found && map_found && out.starts_with("SiiNunit"),
        "Профиль не содержит необходимых полей"
    );
    Ok(out.into_bytes())
}
