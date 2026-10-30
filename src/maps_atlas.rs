use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use ets_launcher::maps::{
    Catalog, SourceState,
    geography::{self, Area, Bounds, Point},
};
use std::collections::{BTreeMap, BTreeSet};
const BASE: Color32 = Color32::from_rgb(62, 120, 112);
const DLC: Color32 = Color32::from_rgb(65, 130, 196);
const NEW: Color32 = Color32::from_rgb(242, 172, 77);
const REWORK: Color32 = Color32::from_rgb(176, 128, 232);
const VIEW: Bounds = [-26.0, 23.0, 95.0, 73.0];
struct Region {
    points: Vec<Point>,
    triangles: Vec<u32>,
    layer: String,
    label: String,
    rework: bool,
}
#[derive(Default)]
pub struct Atlas {
    regions: Vec<Region>,
    revision: Option<u64>,
    zoom: f32,
    pan: Vec2,
    show_all: bool,
    all_dlcs: bool,
    error: Option<String>,
}
impl Atlas {
    fn rebuild(&mut self, catalog: &Catalog) {
        self.regions.clear();
        let result = (|| -> anyhow::Result<()> {
            let countries = geography::countries()?;
            for country in &countries {
                for polygon in &country.polygons {
                    self.add(polygon, VIEW, "land", &country.name, false);
                }
            }
            let mut add_areas = |id: &str, name: &str, areas: &[Area]| {
                for area in areas {
                    if let Some(country) = countries.iter().find(|c| c.id == area.country) {
                        for polygon in &country.polygons {
                            let b = area.bounds.unwrap_or(VIEW);
                            self.add(
                                polygon,
                                [
                                    b[0].max(VIEW[0]),
                                    b[1].max(VIEW[1]),
                                    b[2].min(VIEW[2]),
                                    b[3].min(VIEW[3]),
                                ],
                                id,
                                &format!("{} · {}", country.name, name),
                                area.rework,
                            );
                        }
                    }
                }
            };
            for layer in geography::game_layers()? {
                add_areas(&layer.id, &layer.name, &layer.areas);
            }
            for entry in &catalog.maps {
                if !entry.support
                    && let Some(coverage) = &entry.coverage
                {
                    add_areas(&entry.id, &entry.name, &coverage.areas);
                }
            }
            Ok(())
        })();
        self.error = result.err().map(|e| e.to_string());
        self.revision = Some(catalog.revision);
        if self.zoom == 0.0 {
            self.zoom = 1.0;
            self.show_all = true;
        }
    }
    fn add(&mut self, points: &[Point], bounds: Bounds, layer: &str, label: &str, rework: bool) {
        let points = geography::clip(points, bounds)
            .into_iter()
            .map(geography::mercator)
            .collect::<Vec<_>>();
        if points.len() >= 3 {
            self.regions.push(Region {
                triangles: geography::triangles(&points),
                points,
                layer: layer.into(),
                label: label.into(),
                rework,
            });
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        catalog: &Catalog,
        selected: &BTreeSet<String>,
        focused: Option<&str>,
        dlcs: &BTreeSet<String>,
        sources: &BTreeMap<String, SourceState>,
    ) -> Option<String> {
        if self.revision != Some(catalog.revision) {
            self.rebuild(catalog);
        }
        if let Some(error) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, error);
            return None;
        }
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.show_all, "Все моды");
            ui.checkbox(&mut self.all_dlcs, "Все DLC");
            if ui.button("−").clicked() {
                self.zoom = (self.zoom / 1.3).max(1.0);
            }
            if ui.button("+").clicked() {
                self.zoom = (self.zoom * 1.3).min(8.0);
            }
            if ui.button("Вся карта").clicked() {
                self.zoom = 1.0;
                self.pan = Vec2::ZERO;
            }
            ui.label(
                RichText::new("Колесо: масштаб · перетащить: перемещение · нажать: выбрать")
                    .size(11.0)
                    .weak(),
            );
        });
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(
                ui.available_width(),
                (ui.ctx().content_rect().height() * 0.40).clamp(280.0, 460.0),
            ),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, Color32::from_rgb(15, 28, 42));
        if response.dragged() {
            self.pan += response.drag_delta();
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
                let old = self.zoom;
                self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(1.0, 8.0);
                if let Some(p) = response.hover_pos() {
                    self.pan =
                        (self.pan - (p - rect.center())) * (self.zoom / old) + (p - rect.center());
                }
            }
        }
        self.pan = self
            .pan
            .clamp(-rect.size() * self.zoom, rect.size() * self.zoom);
        let lo = geography::mercator([VIEW[0], VIEW[1]]);
        let hi = geography::mercator([VIEW[2], VIEW[3]]);
        let scale = (rect.width() as f64 / (hi[0] - lo[0]))
            .min(rect.height() as f64 / (hi[1] - lo[1]))
            * self.zoom as f64;
        let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        let screen = |p: Point| {
            rect.center()
                + self.pan
                + Vec2::new(
                    ((p[0] - mid[0]) * scale) as f32,
                    (-(p[1] - mid[1]) * scale) as f32,
                )
        };
        let pointer = response.hover_pos().map(|p| {
            [
                (p.x - rect.center().x - self.pan.x) as f64 / scale + mid[0],
                -(p.y - rect.center().y - self.pan.y) as f64 / scale + mid[1],
            ]
        });
        let mut hovered = Vec::new();
        for region in &self.regions {
            let is_mod = !matches!(region.layer.as_str(), "land" | "base")
                && !region.layer.starts_with("dlc_");
            if is_mod
                && !self.show_all
                && !selected.contains(&region.layer)
                && focused != Some(&region.layer)
            {
                continue;
            }
            let (color, outline) = match region.layer.as_str() {
                "land" => (Color32::from_rgb(36, 49, 61), Color32::from_rgb(69, 84, 95)),
                "base" => (BASE, Color32::from_rgb(94, 159, 148)),
                id if id.starts_with("dlc_") => {
                    if !dlcs.contains(id) && !self.all_dlcs {
                        continue;
                    }
                    if dlcs.contains(id) {
                        (DLC, Color32::from_rgb(93, 170, 221))
                    } else {
                        (
                            Color32::from_rgb(37, 66, 91),
                            Color32::from_rgb(69, 101, 127),
                        )
                    }
                }
                _ => {
                    let c = if region.rework { REWORK } else { NEW };
                    let alpha = if focused == Some(&region.layer) {
                        175
                    } else if selected.contains(&region.layer) {
                        115
                    } else {
                        65
                    };
                    (
                        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha),
                        c,
                    )
                }
            };
            let positions = region
                .points
                .iter()
                .copied()
                .map(screen)
                .collect::<Vec<_>>();
            let mesh = egui::Mesh {
                vertices: positions
                    .iter()
                    .copied()
                    .map(|pos| egui::epaint::Vertex {
                        pos,
                        uv: egui::epaint::WHITE_UV,
                        color,
                    })
                    .collect(),
                indices: region.triangles.clone(),
                ..Default::default()
            };
            painter.add(egui::Shape::mesh(mesh));
            painter.add(egui::Shape::closed_line(
                positions,
                Stroke::new(
                    if focused == Some(&region.layer) {
                        2.0
                    } else {
                        0.7_f32
                    },
                    outline,
                ),
            ));
            if pointer.is_some_and(|p| geography::contains(&region.points, p)) {
                hovered.push(region);
            }
        }
        for (text, location) in [
            ("ЕВРОПА", [13.0, 51.0]),
            ("РОССИЯ", [56.0, 64.0]),
            ("КАЗАХСТАН", [63.0, 48.0]),
            ("СЕВЕРНАЯ АФРИКА", [8.0, 28.0]),
        ] {
            let pos = screen(geography::mercator(location));
            if rect.contains(pos) {
                painter.text(
                    pos,
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(11.0),
                    Color32::from_white_alpha(125),
                );
            }
        }
        painter.text(
            rect.left_bottom() + Vec2::new(12.0, -12.0),
            egui::Align2::LEFT_BOTTOM,
            "Made with Natural Earth",
            egui::FontId::proportional(10.0),
            Color32::from_gray(145),
        );
        if let Some(entry) = focused
            .and_then(|id| catalog.entry(id).ok())
            .filter(|m| m.coverage.as_ref().is_some_and(|c| c.fictional))
        {
            let b = egui::Rect::from_min_size(
                rect.left_top() + Vec2::splat(18.0),
                Vec2::new(260.0, 90.0),
            );
            painter.rect_filled(b, 8.0, Color32::from_rgb(37, 34, 60));
            painter.text(
                b.center(),
                egui::Align2::CENTER_CENTER,
                format!(
                    "{}\nСамостоятельный вымышленный мир\nБез положения на реальной карте",
                    entry.name
                ),
                egui::FontId::proportional(13.0),
                REWORK,
            );
        }
        let picked = if response.clicked() {
            hovered
                .iter()
                .rev()
                .find(|r| catalog.entry(&r.layer).is_ok())
                .map(|r| r.layer.clone())
        } else {
            None
        };
        if !hovered.is_empty() {
            response.on_hover_ui(|ui| {
                let mut seen = BTreeSet::new();
                for region in hovered {
                    if !seen.insert(&region.label) {
                        continue;
                    }
                    ui.label(&region.label);
                    if let Some(size) = sources.get(&region.layer).and_then(|s| s.download_size) {
                        ui.small(format!("Загрузка: {}", ets_launcher::format_bytes(size)));
                    }
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            for (c, text) in [
                (BASE, "Базовая ETS2"),
                (DLC, "Установленные DLC"),
                (NEW, "Новые регионы"),
                (REWORK, "Переработка / доставки"),
            ] {
                ui.colored_label(c, "■");
                ui.label(RichText::new(text).size(12.0));
            }
        });
        ui.label(RichText::new("Региональная схема: границы покрытия приблизительные, дорожная сеть не показана. Стыковки проверяются отдельно для выбранной сборки.").size(11.0).weak());
        picked
    }
}
