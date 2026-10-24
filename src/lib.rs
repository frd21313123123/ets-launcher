pub mod archive;
pub mod catalog;
pub mod download;
pub mod game;
pub mod install;
pub mod playground;
pub mod settings;

pub const SOURCE_PAGE: &str = "https://www.playground.ru/euro_truck_simulator_2/cheat/ets_2_dlc_unlocker_razblokirovschik_dopolnenij_1_61_1_9s_steam_nicholaspooh-1660372";
pub const ATS_SOURCE_PAGE: &str = "https://www.playground.ru/american_truck_simulator/cheat/ats_dlc_unlocker_razblokirovschik_dopolnenij_1_61_3_18s_1_62_0_4s_beta_steam_nicholaspooh-1660567";
pub const TEST_URL: &str = "https://codeload.github.com/octocat/Hello-World/zip/refs/heads/master";
pub const TEST_FILENAME: &str = "hello-world-test.zip";

pub fn format_bytes(bytes: u64) -> String {
    let units = ["Б", "КиБ", "МиБ", "ГиБ", "ТиБ"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} Б")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}
