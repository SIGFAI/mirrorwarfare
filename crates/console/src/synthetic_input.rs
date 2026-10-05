use input_iw4::{command_id_lookup, command_name};

use std::collections::BTreeSet;

use bevy::prelude::Resource;

#[derive(Resource, Debug, Default, Clone)]
pub struct ConsoleInputState {
    held: BTreeSet<u32>,
    timed: Vec<(u32, f32)>,

    pending_mouse: Option<(f32, f32)>,

    mouse_rate: Option<(f32, f32)>,

    turns: Vec<ScriptedTurn>,
    /// Sim tick last seen by `advance_turns` and the frame time since it began.
    turn_clock: Option<(u32, f32)>,

    /// View degrees (yaw, pitch) queued by the autopilot for this frame.
    pending_view: (f32, f32),
}

/// Easing of a scripted `turn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEase {
    Linear,
    /// Sine in-out: starts and ends at rest.
    Smooth,
    /// In-out with a slight (4 %) overshoot that settles back, like a hand on a mouse.
    Settle,
}

impl TurnEase {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "linear" => Some(Self::Linear),
            "smooth" | "ease" => Some(Self::Smooth),
            "settle" | "back" => Some(Self::Settle),
            _ => None,
        }
    }

    /// Fraction of the turn done at `t` in [0, 1]; 0 at 0 and 1 at 1.
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Smooth => 0.5 - 0.5 * (std::f32::consts::PI * t).cos(),
            Self::Settle => {
                // Sine in-out to 4 % past the target by 80 % of the time, then back.
                let smooth = |u: f32| 0.5 - 0.5 * (std::f32::consts::PI * u).cos();
                if t < 0.8 {
                    1.04 * smooth(t / 0.8)
                } else {
                    1.04 - 0.04 * smooth((t - 0.8) / 0.2)
                }
            }
        }
    }
}

/// A view turn spread over time (`turn` console verb): degrees of yaw (positive turns left,
/// as IW4 yaw grows counter-clockwise) and pitch (positive looks down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScriptedTurn {
    pub yaw: f32,
    pub pitch: f32,
    pub seconds: f32,
    pub ease: TurnEase,
    elapsed: f32,
    start: Option<f64>,
    done: (f32, f32),
}

pub const PRESS_SECONDS: f32 = 0.15;

pub const PRESS_TICK_DT_MAX: f32 = 0.05;

/// Simulation tick length that tick-driven `turn`s advance by (sim::MATCH_TICK_MS).
pub const TURN_TICK_SECONDS: f32 = 0.05;

fn plus_command_id(name: &str) -> Option<u32> {
    command_id_lookup(name).map(input_iw4::hold_down_id)
}

impl ConsoleInputState {
    pub fn hold(&mut self, input: &str) -> bool {
        let Some(id) = plus_command_id(input) else {
            return false;
        };
        self.timed.retain(|(held, _)| *held != id);
        self.held.insert(id)
    }

    pub fn press(&mut self, input: &str, seconds: f32) -> bool {
        let Some(id) = plus_command_id(input) else {
            return false;
        };
        self.held.insert(id);
        match self.timed.iter_mut().find(|(held, _)| *held == id) {
            Some(slot) => slot.1 = slot.1.max(seconds),
            None => self.timed.push((id, seconds)),
        }
        true
    }

    pub fn tick(&mut self, dt: f32) {
        let dt = dt.clamp(0.0, PRESS_TICK_DT_MAX);
        for (_, remaining) in self.timed.iter_mut() {
            *remaining -= dt;
        }
        for (id, _) in self.timed.iter().filter(|(_, left)| *left <= 0.0) {
            self.held.remove(id);
        }
        self.timed.retain(|(_, left)| *left > 0.0);
    }

    pub fn release(&mut self, input: &str) -> bool {
        let Some(id) = plus_command_id(input) else {
            return false;
        };
        self.timed.retain(|(held, _)| *held != id);
        self.held.remove(&id)
    }

    pub fn clear(&mut self) {
        self.held.clear();
        self.timed.clear();
        self.pending_mouse = None;
        self.mouse_rate = None;
        self.turns.clear();
        self.pending_view = (0.0, 0.0);
    }

    /// Start a turn of `yaw` / `pitch` degrees over `seconds` (runs alongside any other).
    pub fn turn(&mut self, yaw: f32, pitch: f32, seconds: f32, ease: TurnEase) {
        self.turns.push(ScriptedTurn {
            yaw,
            pitch,
            seconds: seconds.max(0.0),
            ease,
            elapsed: 0.0,
            start: None,
            done: (0.0, 0.0),
        });
    }

    /// Queue a view change in degrees (+yaw left, +pitch down) for the next client input.
    pub fn queue_view(&mut self, yaw: f32, pitch: f32) {
        self.pending_view.0 += yaw;
        self.pending_view.1 += pitch;
    }

    pub fn take_view(&mut self) -> (f32, f32) {
        std::mem::take(&mut self.pending_view)
    }

    pub fn turning(&self) -> bool {
        !self.turns.is_empty()
    }

    /// Advance every scripted turn; the (yaw, pitch) degrees due this frame. With a sim
    /// `tick` (50 ms) the turn runs on simulation time - whole ticks plus the frame time
    /// since the current tick began (capped at one tick) - so the view moves every rendered
    /// frame like a real mouse, while the angle at each tick boundary does not depend on
    /// the frame rate or drift with frame hitches. Without a tick it follows `dt`.
    /// Each turn delivers exactly its total by the time it ends.
    pub fn advance_turns(&mut self, dt: f32, tick: Option<u32>) -> (f32, f32) {
        let now = match tick {
            Some(t) => {
                let sub = match self.turn_clock {
                    Some((last, sub)) if last == t => (sub + dt.max(0.0)).min(TURN_TICK_SECONDS),
                    _ => 0.0,
                };
                self.turn_clock = Some((t, sub));
                Some(t as f64 * f64::from(TURN_TICK_SECONDS) + f64::from(sub))
            }
            None => {
                self.turn_clock = None;
                None
            }
        };
        let mut out = (0.0, 0.0);
        for turn in &mut self.turns {
            match now {
                Some(now) => {
                    let start = *turn.start.get_or_insert(now);
                    turn.elapsed = turn.elapsed.max((now - start) as f32);
                }
                None => turn.elapsed += dt.max(0.0),
            }
            let f = if turn.seconds <= 0.0 {
                1.0
            } else {
                turn.ease.at(turn.elapsed / turn.seconds)
            };
            let want = (turn.yaw * f, turn.pitch * f);
            out.0 += want.0 - turn.done.0;
            out.1 += want.1 - turn.done.1;
            turn.done = want;
        }
        self.turns.retain(|t| t.elapsed < t.seconds);
        out
    }

    pub fn queue_mouse(&mut self, dx: f32, dy: f32) {
        match &mut self.pending_mouse {
            Some((x, y)) => {
                *x += dx;
                *y += dy;
            }
            None => self.pending_mouse = Some((dx, dy)),
        }
    }

    pub fn take_mouse(&mut self) -> (f32, f32) {
        self.pending_mouse.take().unwrap_or((0.0, 0.0))
    }

    pub fn mouse_rate(&self) -> Option<(f32, f32)> {
        self.mouse_rate
    }

    pub fn set_mouse_rate(&mut self, dx: f32, dy: f32) {
        self.mouse_rate = if dx == 0.0 && dy == 0.0 {
            None
        } else {
            Some((dx, dy))
        };
    }

    pub fn held(&self, input: &str) -> bool {
        plus_command_id(input).is_some_and(|id| self.held.contains(&id))
    }

    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.held.iter().copied()
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.held.iter().filter_map(|id| command_name(*id))
    }
}

pub(crate) fn is_bind_command(word: &str) -> bool {
    command_id_lookup(word).is_some()
}

#[cfg(test)]
mod turn_tests {
    use super::*;

    #[test]
    fn turn_delivers_its_total_on_ticks_and_frames() {
        for ease in [TurnEase::Linear, TurnEase::Smooth, TurnEase::Settle] {
            let mut s = ConsoleInputState::default();
            s.turn(90.0, -10.0, 0.8, ease);
            let mut sum = (0.0, 0.0);
            let mut max_step: f32 = 0.0;
            for frame in 0..400 {
                let d = s.advance_turns(0.0071, Some(100 + frame / 7));
                max_step = max_step.max(d.0.abs());
                sum = (sum.0 + d.0, sum.1 + d.1);
            }
            assert!((sum.0 - 90.0).abs() < 1e-3 && (sum.1 + 10.0).abs() < 1e-3, "{ease:?} {sum:?}");
            assert!(!s.turning());
            assert!(max_step < 3.0, "{ease:?} per-frame step {max_step}");
            let mut s = ConsoleInputState::default();
            s.turn(-45.0, 0.0, 0.5, ease);
            let mut sum = 0.0;
            for _ in 0..100 {
                sum += s.advance_turns(0.0137, None).0;
            }
            assert!((sum + 45.0).abs() < 1e-3);
        }
        assert_eq!(TurnEase::Settle.at(0.0), 0.0);
        assert!((TurnEase::Settle.at(1.0) - 1.0).abs() < 1e-6);
        assert!((0.0..=1.1).contains(&TurnEase::Settle.at(0.9)));
    }
}
