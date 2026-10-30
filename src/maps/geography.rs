//! Approximate regional coverage, separate from the game's road/sector data.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub type Point = [f64; 2];
pub type Bounds = [f64; 4];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Coverage {
    pub summary: String,
    pub source: String,
    #[serde(default)]
    pub fictional: bool,
    #[serde(default)]
    pub areas: Vec<Area>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Area {
    pub country: String,
    /// Optional west/south/east/north clipping box for a partial country.
    #[serde(default)]
    pub bounds: Option<Bounds>,
    #[serde(default)]
    pub rework: bool,
}

impl Coverage {
    pub fn validate(&self) -> Result<()> {
        super::https(&self.source)?;
        ensure!(
            self.summary.len() <= 2000 && self.areas.len() <= 100,
            "Слишком много данных покрытия"
        );
        for area in &self.areas {
            ensure!(
                area.country.len() == 3 && area.country.bytes().all(|b| b.is_ascii_uppercase()),
                "Некорректная страна покрытия"
            );
            if let Some([w, s, e, n]) = area.bounds {
                ensure!(
                    [w, s, e, n].iter().all(|v| v.is_finite())
                        && w < e
                        && s < n
                        && w >= -180.0
                        && e <= 180.0
                        && s >= -85.0
                        && n <= 85.0,
                    "Некорректные границы покрытия"
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Country {
    pub id: String,
    pub name: String,
    pub center: Point,
    pub polygons: Vec<Vec<Point>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GameLayer {
    pub id: String,
    pub name: String,
    pub areas: Vec<Area>,
}

pub fn countries() -> Result<Vec<Country>> {
    Ok(serde_json::from_str(include_str!(
        "../../assets/geography/countries.json"
    ))?)
}

pub fn game_layers() -> Result<Vec<GameLayer>> {
    Ok(serde_json::from_str(include_str!(
        "../../assets/geography/game-layers.json"
    ))?)
}

/// Sutherland-Hodgman clipping in geographic coordinates. Used for schematic
/// partial-country coverage; no percentage of actual drivable roads is inferred.
pub fn clip(points: &[Point], bounds: Bounds) -> Vec<Point> {
    let mut polygon = points.to_vec();
    if polygon.first() == polygon.last() {
        polygon.pop();
    }
    for (edge, &limit) in bounds.iter().enumerate() {
        let input = std::mem::take(&mut polygon);
        if input.is_empty() {
            break;
        }
        let axis = edge % 2;
        let inside = |p: Point| {
            if edge < 2 {
                p[axis] >= limit
            } else {
                p[axis] <= limit
            }
        };
        let mut previous = *input.last().unwrap();
        for next in input {
            if inside(previous) != inside(next) {
                let t = (limit - previous[axis]) / (next[axis] - previous[axis]);
                let mut p = [
                    previous[0] + t * (next[0] - previous[0]),
                    previous[1] + t * (next[1] - previous[1]),
                ];
                p[axis] = limit;
                polygon.push(p);
            }
            if inside(next) {
                polygon.push(next);
            }
            previous = next;
        }
    }
    polygon.dedup_by(|a, b| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() < 1e-9);
    polygon
}

pub fn contains(polygon: &[Point], p: Point) -> bool {
    let mut inside = false;
    let Some(mut previous) = polygon.last().copied() else {
        return false;
    };
    for &next in polygon {
        if (next[1] > p[1]) != (previous[1] > p[1])
            && p[0] < (previous[0] - next[0]) * (p[1] - next[1]) / (previous[1] - next[1]) + next[0]
        {
            inside = !inside;
        }
        previous = next;
    }
    inside
}

fn cross(a: Point, b: Point, c: Point) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Ear clipping preserves concave coastlines (a convex fan would fill seas).
pub fn triangles(points: &[Point]) -> Vec<u32> {
    if points.len() < 3 {
        return Vec::new();
    }
    let signed: f64 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
        .sum();
    let mut remaining: Vec<usize> = (0..points.len()).collect();
    if signed < 0.0 {
        remaining.reverse();
    }
    let mut output = Vec::new();
    while remaining.len() >= 3 {
        let mut found = false;
        for i in 0..remaining.len() {
            let a = remaining[(i + remaining.len() - 1) % remaining.len()];
            let b = remaining[i];
            let c = remaining[(i + 1) % remaining.len()];
            if cross(points[a], points[b], points[c]) <= 1e-10 {
                continue;
            }
            if remaining
                .iter()
                .copied()
                .filter(|&v| v != a && v != b && v != c)
                .any(|v| {
                    cross(points[a], points[b], points[v]) >= -1e-10
                        && cross(points[b], points[c], points[v]) >= -1e-10
                        && cross(points[c], points[a], points[v]) >= -1e-10
                })
            {
                continue;
            }
            output.extend([a as u32, b as u32, c as u32]);
            remaining.remove(i);
            found = true;
            break;
        }
        if !found {
            break;
        } // Degenerate boundary: draw outline, never fan-fill it.
    }
    output
}

pub fn mercator(p: Point) -> Point {
    [
        p[0].to_radians(),
        (std::f64::consts::FRAC_PI_4 + p[1].clamp(-80.0, 80.0).to_radians() / 2.0)
            .tan()
            .ln(),
    ]
}
