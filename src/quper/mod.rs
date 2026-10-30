//! Ready-made combos published by Quper. Metadata, file lists and mod order
//! come from the author's site; no JavaScript from the site is executed.
pub mod files;
pub mod library;
pub mod source;

use crate::game::Game;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const SITE: &str = "https://qupersimulator.ru/";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Combo {
    pub id: String,
    pub title: String,
    pub game: Game,
    pub game_version: String,
    pub version: String,
    pub date: String,
    pub preview: String,
    pub page: String,
    pub table: String,
    pub automatic_order: bool,
    pub convoy: bool,
}

impl Combo {
    pub fn validate(&self) -> Result<()> {
        crate::maps::component(&self.id)?;
        ensure!(
            !self.title.trim().is_empty() && self.title.len() <= 300,
            "Некорректное название сборки"
        );
        ensure!(
            !self.version.is_empty() && self.version.len() <= 100,
            "Нет версии сборки"
        );
        ensure!(
            regex::Regex::new(r"^1\.\d{2,3}$")?.is_match(&self.game_version),
            "Нет версии игры"
        );
        source::site_url(&self.preview)?;
        source::site_url(&self.page)?;
        source::sheet_url(&self.table)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub fetched_at: u64,
    pub combos: Vec<Combo>,
}

impl Catalog {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.combos.is_empty() && self.combos.len() <= 1000,
            "Каталог Quper пуст или имеет неизвестный формат"
        );
        let mut ids = BTreeSet::new();
        for combo in &self.combos {
            combo.validate()?;
            ensure!(ids.insert(&combo.id), "Повторяющаяся сборка Quper");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRow {
    pub number: u32,
    pub name: String,
    pub description: String,
    pub mod_id: String,
    /// Public author links only. Boosty URLs are discarded before queuing.
    pub urls: Vec<String>,
    pub excluded_boosty: bool,
    pub required: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub combo: Combo,
    pub files: Vec<FileRow>,
    /// Low priority first, in the exact order supplied by the author.
    pub order: Vec<String>,
}

impl Plan {
    pub fn validate(&self) -> Result<()> {
        self.combo.validate()?;
        ensure!(
            !self.files.is_empty() && self.files.len() <= 2000,
            "Не найден список файлов сборки"
        );
        ensure!(
            !self.order.is_empty() && self.order.len() <= 2000,
            "Не найден порядок модов сборки"
        );
        let mut ids = BTreeSet::new();
        for id in &self.order {
            valid_mod_id(id)?;
            ensure!(
                ids.insert(id.to_lowercase()),
                "Повторяющийся мод в порядке Quper: {id}"
            );
        }
        for row in &self.files {
            valid_mod_id(&row.mod_id)?;
            for url in &row.urls {
                source::public_url(url)?;
            }
        }
        Ok(())
    }
}

pub fn valid_mod_id(id: &str) -> Result<()> {
    ensure!(
        !id.trim().is_empty() && id.len() <= 240 && !id.contains('|'),
        "Некорректный идентификатор мода"
    );
    let path = crate::archive::safe_relative(id)?;
    ensure!(path.components().count() == 1, "Недопустимый путь мода");
    Ok(())
}
