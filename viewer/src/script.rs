//! Scripted input for testing the viewer by hand-free runs (built only
//! with the `script` feature): `k-pdf file.pdf --script steps.txt` plays
//! the steps, one per line, then closes the window.
//!
//! ```text
//! click 300 250        # left click at window point (x, y)
//! drag 260 257 430 276 # press, move, release
//! press 300 250        # or step by step (a shot can come in between)
//! release 320 260
//! move 300 250
//! key ctrl+shift+h     # modifiers: ctrl (Cmd on macOS), shift, alt
//! text Hello there     # typed text
//! wait 10              # frames
//! pause 1500           # milliseconds (for work in the background)
//! shot out.ppm         # once everything is rendered, save the window
//! ```

use std::collections::VecDeque;
use std::path::PathBuf;

use eframe::egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput};

pub enum Step {
    Event(Event),
    Modifiers(Modifiers),
    Wait(u32),
    /// Wall-clock milliseconds (frames can take well under a millisecond,
    /// so `wait` is too short for background work).
    Pause(u64),
    Shot(PathBuf),
}

pub struct Script {
    steps: VecDeque<Step>,
    pointer: Pos2,
    modifiers: Modifiers,
    wait: u32,
    until: Option<std::time::Instant>,
}

impl Script {
    pub fn load(path: &std::path::Path) -> Result<Script, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut steps = VecDeque::new();
        let mut pointer = Pos2::ZERO;
        let add = |steps: &mut VecDeque<Step>, e: Event| {
            steps.push_back(Step::Event(e));
            steps.push_back(Step::Wait(2));
        };
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (op, rest) = line.split_once(' ').unwrap_or((line, ""));
            let nums: Vec<f32> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            let button = |pos, pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            match (op, &nums[..]) {
                ("move", [x, y]) => {
                    pointer = Pos2::new(*x, *y);
                    add(&mut steps, Event::PointerMoved(pointer));
                }
                ("click", [x, y]) => {
                    pointer = Pos2::new(*x, *y);
                    add(&mut steps, Event::PointerMoved(pointer));
                    add(&mut steps, button(pointer, true));
                    add(&mut steps, button(pointer, false));
                }
                ("press" | "release", [x, y]) => {
                    pointer = Pos2::new(*x, *y);
                    add(&mut steps, Event::PointerMoved(pointer));
                    add(&mut steps, button(pointer, op == "press"));
                }
                ("drag", [x0, y0, x1, y1]) => {
                    let from = Pos2::new(*x0, *y0);
                    add(&mut steps, Event::PointerMoved(from));
                    add(&mut steps, button(from, true));
                    for i in 1..=8 {
                        pointer = from + (Pos2::new(*x1, *y1) - from) * (i as f32 / 8.0);
                        add(&mut steps, Event::PointerMoved(pointer));
                    }
                    add(&mut steps, button(pointer, false));
                }
                ("key", _) => {
                    let mut modifiers = Modifiers::NONE;
                    let mut key = None;
                    for part in rest.split('+') {
                        match part.trim().to_lowercase().as_str() {
                            "ctrl" | "cmd" => modifiers |= Modifiers::COMMAND,
                            "shift" => modifiers |= Modifiers::SHIFT,
                            "alt" => modifiers |= Modifiers::ALT,
                            other => {
                                let mut capital = other.to_string();
                                capital[..1].make_ascii_uppercase();
                                key = Key::from_name(other)
                                    .or_else(|| Key::from_name(&capital))
                                    .or_else(|| Key::from_name(&other.to_uppercase()));
                            }
                        }
                    }
                    let key = key.ok_or_else(|| format!("line {}: unknown key in {rest:?}", n + 1))?;
                    steps.push_back(Step::Modifiers(modifiers));
                    for pressed in [true, false] {
                        add(&mut steps, Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
                    }
                    steps.push_back(Step::Modifiers(Modifiers::NONE));
                }
                ("text", _) => add(&mut steps, Event::Text(rest.to_string())),
                ("wait", [frames]) => steps.push_back(Step::Wait(*frames as u32)),
                ("pause", [ms]) => steps.push_back(Step::Pause(*ms as u64)),
                ("shot", _) if !rest.is_empty() => steps.push_back(Step::Shot(PathBuf::from(rest))),
                _ => return Err(format!("line {}: can't read {line:?}", n + 1)),
            }
        }
        Ok(Script { steps, pointer: Pos2::ZERO, modifiers: Modifiers::NONE, wait: 0, until: None })
    }

    /// Adds this frame's scripted input. Returns a screenshot to take
    /// (the script waits until the viewer has taken it), or `Done` at the
    /// end.
    pub fn feed(&mut self, raw: &mut RawInput) -> Next {
        raw.modifiers = self.modifiers;
        if self.wait > 0 {
            self.wait -= 1;
            return Next::Continue;
        }
        if let Some(until) = self.until {
            if std::time::Instant::now() < until {
                return Next::Continue;
            }
            self.until = None;
        }
        while let Some(step) = self.steps.pop_front() {
            match step {
                Step::Event(e) => {
                    if let Event::PointerMoved(p) = e {
                        self.pointer = p;
                    }
                    let e = match e {
                        Event::PointerButton { pos, button, pressed, .. } => {
                            Event::PointerButton { pos, button, pressed, modifiers: self.modifiers }
                        }
                        other => other,
                    };
                    raw.events.push(e);
                }
                Step::Modifiers(m) => {
                    self.modifiers = m;
                    raw.modifiers = m;
                }
                Step::Wait(n) => {
                    self.wait = n;
                    return Next::Continue;
                }
                Step::Pause(ms) => {
                    self.until = Some(std::time::Instant::now() + std::time::Duration::from_millis(ms));
                    return Next::Continue;
                }
                Step::Shot(path) => return Next::Shot(path),
            }
        }
        Next::Done
    }
}

pub enum Next {
    Continue,
    Shot(PathBuf),
    Done,
}
