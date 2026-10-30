//! Debug-only UI automation for checking the interface without touching the desktop.
//!
//! `ETS_LAUNCHER_UI_SCRIPT=<file>` replays one command per line into the window:
//! `click X Y`, `move X Y`, `scroll DY`, `type TEXT`, `backspace N`, `wait MS`, `shot FILE.png`, `exit`.
//! Coordinates are egui points; screenshots are saved from the window's own frame.

use eframe::egui::{self, Event, Modifiers, PointerButton, Pos2, RawInput, pos2, vec2};
use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};

enum Step {
    Click(Pos2),
    Move(Pos2),
    Scroll(f32),
    Type(String),
    Backspace(usize),
    Wait(Duration),
    Shot(PathBuf),
    Exit,
}

pub struct Script {
    steps: VecDeque<Step>,
    resume: Instant,
    shot: Option<PathBuf>,
    release: Option<Pos2>,
}

impl Script {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("ETS_LAUNCHER_UI_SCRIPT")?;
        let source = std::fs::read_to_string(path).ok()?;
        let number = |value: Option<&str>| value.and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
        let steps = source
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| {
                let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
                let mut args = rest.split_whitespace();
                Some(match command {
                    "click" => Step::Click(pos2(number(args.next()), number(args.next()))),
                    "move" => Step::Move(pos2(number(args.next()), number(args.next()))),
                    "scroll" => Step::Scroll(number(args.next())),
                    "type" => Step::Type(rest.to_owned()),
                    "backspace" => Step::Backspace(number(args.next()) as usize),
                    "wait" => Step::Wait(Duration::from_millis(number(args.next()) as u64)),
                    "shot" => Step::Shot(PathBuf::from(rest)),
                    "exit" => Step::Exit,
                    _ => return None,
                })
            })
            .collect();
        Some(Self {
            steps,
            resume: Instant::now() + Duration::from_millis(1500),
            shot: None,
            release: None,
        })
    }

    pub fn input(&mut self, ctx: &egui::Context, raw: &mut RawInput) {
        ctx.request_repaint_after(Duration::from_millis(30));
        for event in &raw.events {
            if let Event::Screenshot { image, .. } = event
                && let Some(path) = self.shot.take()
            {
                let pixels = image
                    .pixels
                    .iter()
                    .flat_map(|c| c.to_array())
                    .collect::<Vec<_>>();
                if let Some(buffer) =
                    image::RgbaImage::from_raw(image.size[0] as u32, image.size[1] as u32, pixels)
                {
                    let _ = buffer.save(&path);
                }
            }
        }
        if let Some(pos) = self.release.take() {
            raw.events.push(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
            return;
        }
        if self.shot.is_some() || Instant::now() < self.resume {
            return;
        }
        let Some(step) = self.steps.pop_front() else {
            return;
        };
        self.resume = Instant::now() + Duration::from_millis(250);
        match step {
            Step::Click(pos) => {
                raw.events.push(Event::PointerMoved(pos));
                raw.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                });
                self.release = Some(pos);
            }
            Step::Move(pos) => raw.events.push(Event::PointerMoved(pos)),
            Step::Scroll(dy) => raw.events.push(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, dy),
                modifiers: Modifiers::NONE,
            }),
            Step::Type(text) => raw.events.push(Event::Text(text)),
            Step::Backspace(count) => {
                for pressed in std::iter::repeat_n([true, false], count).flatten() {
                    raw.events.push(Event::Key {
                        key: egui::Key::Backspace,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    });
                }
            }
            Step::Wait(duration) => self.resume = Instant::now() + duration,
            Step::Shot(path) => {
                self.shot = Some(path);
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            Step::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
}
