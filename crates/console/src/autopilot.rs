//! Closed-loop route autopilot (`autopilot <route.json> [async|exit]`, `autopilot stop`).
//!
//! A route is data (written by `scripts/mec-testbox.py` next to the arena): a polyline of
//! segments in glTF metres, each with optional actions fired on STATE triggers (distance to
//! the segment point, mec mode, time in mode, falling) and an expected mec mode that
//! validates the action. While it runs the client sends one usercmd per sim tick
//! (`net::FixedInputStep`, lockstep with the listen authority) and the autopilot decides
//! once per command from the predicted local state after the previous one (origin,
//! velocity, mec mode; a wallclimb turned by a quickturn reads `WallClimb180`): it advances
//! along the route, fires actions and checks expectations.
//! Decisions depend on game state only, never on the wall clock or the frame rate, so two
//! runs are identical tick for tick. Movement keys go through the scripted console inputs;
//! the view is a critically damped spring stepped once per command toward a pure-pursuit
//! point on the path (eyes level; a segment's `glance` point only inside `glance_dist`),
//! written into the command angles in whole angle units. The local player is presented
//! interpolated between commands, so the camera still moves every rendered frame.
//!
//! Log lines: `autopilot: seg <i> <id> tick=<t> at=(x,y,z)`, `autopilot: step <seg>/<n> ok
//! mode=<m> tick=<t>` or `autopilot: FAIL ...`, `autopilot: done ok ...`. Without `async` the
//! console FIFO holds until the route ends; with `exit` a failure exits the process with 3.

use std::collections::BTreeSet;

use bevy::prelude::*;
use serde::Deserialize;

use crate::ConsoleInputState;

const IN: f32 = 39.37;
/// Eye height above the feet (IW4 stand view height), metres.
const EYE: f32 = 1.52;
/// Level running gaze: a little down, the ground ~20 m ahead.
const LEVEL_PITCH: f32 = 4.0;
/// Pure-pursuit look-ahead: `max(MIN, speed * TIME)` metres along the path.
const LOOKAHEAD_MIN: f32 = 2.5;
const LOOKAHEAD_TIME: f32 = 0.45;
/// View spring (critically damped), rad/s: yaw follows the path, pitch is slower.
const YAW_OMEGA: f32 = 11.0;
const PITCH_OMEGA: f32 = 7.0;
/// Hands off the mouse like a runner does: no view steering while rolling, on the wall
/// during a quickturn (the game turns the view itself), and through a jump off a wall
/// (wall jump / wall kick) plus this many ticks (0.35 s) after it.
const LOOK_HOLD_AFTER_KICK: u32 = 7;
/// Steering eases back in over this many ticks (0.25 s) once a hold ends.
const LOOK_EASE_IN: u32 = 5;
/// Upward speed (m/s) at leaving a wall that marks a jump off it (not the wallrun running out).
const KICK_RISE: f32 = 3.0;
/// A decision lands on the next command (one tick after the state it read): distance
/// triggers and waypoint arrival use the position extrapolated one tick ahead.
const LEAD_S: f32 = 0.05;

#[derive(Debug, Clone, Deserialize)]
pub struct Route {
    #[serde(default)]
    pub name: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Segment {
    pub id: String,
    /// Waypoint, glTF metres.
    pub to: [f32; 3],
    /// Advance when horizontally this close to `to` (or past it along the segment).
    #[serde(default = "default_reach")]
    pub reach: f32,
    /// Advance only when this holds (instead of reaching `to`).
    #[serde(default)]
    pub until: Option<Cond>,
    /// Point the eyes go to within `glance_dist` of `to` (climb lips, landing zones).
    #[serde(default)]
    pub glance: Option<[f32; 3]>,
    #[serde(default = "default_glance_dist")]
    pub glance_dist: f32,
    /// Keep the yaw on this point instead of the path (a wall being run along, say).
    #[serde(default)]
    pub face: Option<[f32; 3]>,
    /// Pursue only this segment's line (do not cut toward the next waypoint early):
    /// approaches that must arrive straight, like a run beside a wallrun wall.
    #[serde(default)]
    pub hold_line: bool,
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Ticks this segment may take before the run fails as stuck.
    #[serde(default = "default_max_ticks")]
    pub max_ticks: u32,
    /// Caption for the video (logged on entry).
    #[serde(default)]
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Cond {
    /// Horizontal distance to the segment's `to` below this.
    #[serde(default)]
    pub dist_lt: Option<f32>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub not_mode: Option<String>,
    /// Ticks spent in the current mode at least this.
    #[serde(default)]
    pub mode_ticks_ge: Option<u32>,
    /// Ticks since the segment began at least this.
    #[serde(default)]
    pub seg_ticks_ge: Option<u32>,
    /// Vertical velocity below zero (true) / at or above (false).
    #[serde(default)]
    pub falling: Option<bool>,
    /// Feet below this height (metres).
    #[serde(default)]
    pub y_lt: Option<f32>,
    #[serde(default)]
    pub y_gt: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    #[serde(rename = "if", default)]
    pub cond: Cond,
    #[serde(default)]
    pub hold: Vec<String>,
    #[serde(default)]
    pub release: Vec<String>,
    /// Held for exactly one tick.
    #[serde(default)]
    pub tap: Vec<String>,
    /// Quickturn: tap `+quickturn` and turn the autopilot's own view 180 degrees with it.
    #[serde(default)]
    pub quickturn: bool,
    /// Mode the action must produce within `within_ticks` (`A|B` accepts either).
    #[serde(default)]
    pub expect: Option<String>,
    #[serde(default = "default_within")]
    pub within_ticks: u32,
    #[serde(default)]
    pub caption: Option<String>,
}

fn default_reach() -> f32 {
    0.8
}
fn default_glance_dist() -> f32 {
    3.5
}
fn default_max_ticks() -> u32 {
    400
}
fn default_within() -> u32 {
    12
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Idle,
    Running,
    Done,
    Failed,
}

#[derive(Default)]
struct Spring {
    pos: f32,
    vel: f32,
}

impl Spring {
    /// Critically damped step toward `target` (exact for constant target over dt).
    fn step(&mut self, target: f32, omega: f32, dt: f32) {
        let x = self.pos - target;
        let exp = (-omega * dt).exp();
        let tmp = (self.vel + omega * x) * dt;
        self.vel = (self.vel - omega * tmp) * exp;
        self.pos = target + (x + tmp) * exp;
    }
}

#[derive(Resource, Default)]
pub struct Autopilot {
    pub status: Status,
    route: Option<Route>,
    seg: usize,
    seg_start: u32,
    fired: Vec<bool>,
    expects: Vec<(usize, usize, String, u32)>,
    held: BTreeSet<String>,
    taps: Vec<String>,
    /// Keys an action asked to hold while already held: released this tick, held next tick,
    /// so the game sees a fresh press.
    repress: Vec<String>,
    last_tick: Option<u32>,
    mode: String,
    mode_since: u32,
    pos: [f32; 3],
    vel: [f32; 3],
    seg_from: [f32; 3],
    yaw: Spring,
    pitch: Spring,
    target_yaw: f32,
    target_pitch: f32,
    /// FIFO hold: release the console queue when the route ends.
    pub hold_fifo: bool,
    exit_on_fail: bool,
    steps_ok: u32,
    /// Fixed-step commands seen / decided on, and whether this autopilot turned fixed-step on.
    last_emitted: u64,
    decisions: u32,
    owns_fixed: bool,
    /// View angles already written into the command angles (degrees).
    sent: (f32, f32),
    /// View held (no steering) through this tick; steering resumed on `look_resume`.
    look_hold_until: Option<u32>,
    look_resume: Option<u32>,
    look_held: bool,
}

impl Autopilot {
    pub fn start(&mut self, route: Route, hold_fifo: bool, exit_on_fail: bool) {
        let n = route.segments.first().map_or(0, |s| s.actions.len());
        let (last_emitted, owns_fixed) = (self.last_emitted, self.owns_fixed);
        *self = Self {
            status: Status::Running,
            fired: vec![false; n],
            route: Some(route),
            hold_fifo,
            exit_on_fail,
            last_emitted,
            owns_fixed,
            ..Self::default()
        };
    }

    pub fn running(&self) -> bool {
        self.status == Status::Running
    }

    pub fn stop(&mut self, inputs: &mut ConsoleInputState) {
        for h in std::mem::take(&mut self.held) {
            inputs.release(&h);
        }
        for t in std::mem::take(&mut self.taps) {
            inputs.release(&t);
        }
        if self.status == Status::Running {
            self.status = Status::Done;
        }
        self.hold_fifo = false;
    }
}

impl Autopilot {
    fn lead(&self) -> [f32; 3] {
        [self.pos[0] + self.vel[0] * LEAD_S, self.pos[1], self.pos[2] + self.vel[2] * LEAD_S]
    }
}

/// `mec:<arena>` reads `<arena dir>/route.json`, `mec:<arena>/<name>` reads
/// `<arena dir>/routes/<name>.json`; anything else is a file path.
pub fn load_route(path: &str) -> Result<Route, String> {
    let path = match path.strip_prefix("mec:") {
        Some(spec) => match spec.split_once('/') {
            Some((arena, name)) => asset_mec::arena_dir(arena).join("routes").join(format!("{name}.json")),
            None => asset_mec::arena_dir(spec).join("route.json"),
        }
        .to_string_lossy()
        .into_owned(),
        None => path.to_owned(),
    };
    let path = path.as_str();
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let route: Route = serde_json::from_slice(&bytes).map_err(|e| format!("{path}: {e}"))?;
    if route.segments.is_empty() {
        return Err(format!("{path}: route has no segments"));
    }
    Ok(route)
}

fn to_gltf(o: [f32; 3]) -> [f32; 3] {
    [-o[1] / IN, o[2] / IN, -o[0] / IN]
}

/// IW4 yaw (degrees) of a glTF heading: 0 = -Z, +90 = -X.
fn yaw_of(dx: f32, dz: f32) -> f32 {
    (-dx).atan2(-dz).to_degrees()
}

fn wrap(a: f32) -> f32 {
    (a + 180.0).rem_euclid(360.0) - 180.0
}

fn hdist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

impl Cond {
    fn holds(&self, ap: &Autopilot, to: [f32; 3], tick: u32) -> bool {
        self.dist_lt.is_none_or(|d| hdist(ap.lead(), to) < d)
            && self.mode.as_ref().is_none_or(|m| *m == ap.mode)
            && self.not_mode.as_ref().is_none_or(|m| *m != ap.mode)
            && self
                .mode_ticks_ge
                .is_none_or(|n| tick.wrapping_sub(ap.mode_since) >= n)
            && self
                .seg_ticks_ge
                .is_none_or(|n| tick.wrapping_sub(ap.seg_start) >= n)
            && self.falling.is_none_or(|f| (ap.vel[1] < -0.5) == f)
            && self.y_lt.is_none_or(|y| ap.pos[1] < y)
            && self.y_gt.is_none_or(|y| ap.pos[1] > y)
    }
}

/// Point `ahead` metres along the route from the projection of `pos` on the current segment.
fn pursuit_point(route: &Route, seg: usize, from: [f32; 3], pos: [f32; 3], ahead: f32) -> [f32; 3] {
    let mut a = from;
    let mut left = ahead;
    let mut first = true;
    for s in &route.segments[seg..] {
        let b = s.to;
        let (dx, dz) = (b[0] - a[0], b[2] - a[2]);
        let len = (dx * dx + dz * dz).sqrt();
        if len < 1.0e-3 {
            a = b;
            continue;
        }
        let mut t0 = 0.0;
        if first {
            // project the player onto this segment
            t0 = (((pos[0] - a[0]) * dx + (pos[2] - a[2]) * dz) / len).clamp(0.0, len);
            first = false;
        }
        if t0 + left <= len || s.hold_line {
            // past the end of a held line: keep looking along it
            let t = t0 + left;
            return [a[0] + dx / len * t, b[1], a[2] + dz / len * t];
        }
        left -= len - t0;
        a = b;
    }
    a
}

/// Decisions before the route starts acting: the first fixed-step command carries an
/// irregular step from the frame-timed command before it; the runner stands still meanwhile.
const WARMUP_COMMANDS: u32 = 4;
/// Most the view may turn per command (50 ms), degrees: a quick but human flick.
const YAW_STEP_MAX: f32 = 10.0;
const PITCH_STEP_MAX: f32 = 6.0;

#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_autopilot(
    local: Option<Res<net::LocalPresentClient>>,
    prediction: Option<Res<net::ClientPredictionState>>,
    fixed: Option<ResMut<net::FixedInputStep>>,
    look: Option<ResMut<net::LookState>>,
    mut ap: ResMut<Autopilot>,
    mut inputs: ResMut<ConsoleInputState>,
    mut dispatch: ResMut<crate::plugin::ConsoleDispatch>,
    mut exit: MessageWriter<AppExit>,
) {
    let (Some(mut fixed), Some(mut look)) = (fixed, look) else {
        return;
    };
    if !ap.running() {
        // Finished, failed or stopped: the FIFO hold ends (a hold set by the dispatcher
        // waits for the command handler to start the route).
        if dispatch.wait_autopilot && !dispatch.autopilot_pending {
            dispatch.wait_autopilot = false;
        }
        if ap.owns_fixed {
            fixed.enabled = false;
            ap.owns_fixed = false;
        }
        return;
    }
    let (Some(local), Some(prediction)) = (local, prediction) else {
        return;
    };
    // One usercmd per sim tick while driving; decide once per emitted command, from the
    // predicted state after it - so every input is a function of game state alone.
    fixed.enabled = true;
    ap.owns_fixed = true;
    if fixed.emitted == ap.last_emitted {
        return;
    }
    ap.last_emitted = fixed.emitted;
    let Some(&(cmd_ms, ps)) = fixed.current() else {
        return;
    };
    let tick = cmd_ms.div_euclid(net::FIXED_INPUT_STEP_MS) as u32;
    ap.decisions += 1;
    if ap.decisions <= WARMUP_COMMANDS {
        ap.yaw = Spring { pos: ps.viewangles[1], vel: 0.0 };
        ap.pitch = Spring { pos: ps.viewangles[0], vel: 0.0 };
        ap.sent = (ps.viewangles[1], ps.viewangles[0]);
        ap.target_yaw = ps.viewangles[1];
        ap.target_pitch = ps.viewangles[0];
        return;
    }
    let first_tick = ap.last_tick.is_none();
    ap.last_tick = Some(tick);
    for t in std::mem::take(&mut ap.taps) {
        if !ap.held.contains(&t) {
            inputs.release(&t);
        }
    }
    for k in std::mem::take(&mut ap.repress) {
        inputs.hold(&k);
    }
    let mode = prediction
        .0
        .world()
        .client_meta(local.0)
        .and_then(|m| m.mec())
        .map_or_else(|| "-".to_owned(), |m| m.mode_name().to_owned());
    let prev_mode = std::mem::take(&mut ap.mode);
    ap.pos = to_gltf(ps.origin);
    ap.vel = to_gltf(ps.velocity);
    if mode != prev_mode {
        // Jumped off a wall: hold the view through the jump and a little after.
        if mode == "Air" && matches!(prev_mode.as_str(), "WallRun" | "WallClimb180") && ap.vel[1] > KICK_RISE {
            ap.look_hold_until = Some(tick + LOOK_HOLD_AFTER_KICK);
        }
        ap.mode_since = tick;
    }
    ap.mode = mode;
    if first_tick {
        ap.seg_start = tick;
        ap.seg_from = ap.pos;
        log_segment(&ap, tick);
    }

    let Some(route) = ap.route.take() else {
        return;
    };
    let result = step_route(&mut ap, &route, tick, &mut inputs);
    ap.route = Some(route);
    if let Err(msg) = result {
        diag::warn!(Console, "autopilot: FAIL {msg}");
        println!("autopilot: FAIL {msg}");
        ap.stop(&mut inputs);
        ap.status = Status::Failed;
        dispatch.wait_autopilot = false;
        if ap.exit_on_fail {
            exit.write(AppExit::from_code(3));
        }
        return;
    }
    if ap.status == Status::Done {
        let n = ap.steps_ok;
        diag::info!(Console, "autopilot: done ok steps={n} tick={tick}");
        println!("autopilot: done ok steps={n} tick={tick}");
        ap.stop(&mut inputs);
        ap.status = Status::Done;
        dispatch.wait_autopilot = false;
        return;
    }

    // View for the next command: a critically damped spring stepped once per command,
    // written into the command angles as whole angle units (exact, run to run). The
    // presentation interpolates between commands, so the camera moves every frame.
    // Hands off during a roll, the wall quickturn and a wall jump (+ LOOK_HOLD_AFTER_KICK),
    // then ease back in: the step limit ramps up over LOOK_EASE_IN ticks from rest.
    let hold = matches!(ap.mode.as_str(), "Roll" | "WallClimb180") || ap.look_hold_until.is_some_and(|t| tick <= t);
    if hold {
        ap.look_held = true;
        ap.yaw.vel = 0.0;
        ap.pitch.vel = 0.0;
        return;
    }
    if ap.look_held {
        ap.look_held = false;
        ap.look_hold_until = None;
        ap.look_resume = Some(tick);
    }
    let ease = ap.look_resume.map_or(1.0, |t0| {
        let x = ((tick.wrapping_sub(t0) + 1) as f32 / LOOK_EASE_IN as f32).min(1.0);
        x * x * (3.0 - 2.0 * x)
    });
    let dt = net::FIXED_INPUT_STEP_MS as f32 / 1000.0;
    let ty = ap.yaw.pos + wrap(ap.target_yaw - ap.yaw.pos);
    let tp = ap.target_pitch;
    let (y0, p0) = (ap.yaw.pos, ap.pitch.pos);
    ap.yaw.step(ty, YAW_OMEGA, dt);
    ap.pitch.step(tp, PITCH_OMEGA, dt);
    let (ym, pm) = (YAW_STEP_MAX * ease, PITCH_STEP_MAX * ease);
    ap.yaw.pos = y0 + (ap.yaw.pos - y0).clamp(-ym, ym);
    ap.pitch.pos = p0 + (ap.pitch.pos - p0).clamp(-pm, pm);
    let short = |deg: f32| (deg * input_iw4::ANGLE2SHORT).round() as i32;
    let dyaw = short(ap.yaw.pos) - short(ap.sent.0);
    let dpitch = short(ap.pitch.pos) - short(ap.sent.1);
    look.angles[1] = look.angles[1].wrapping_add(dyaw);
    look.angles[0] = look.angles[0].wrapping_add(dpitch);
    ap.sent = (ap.yaw.pos, ap.pitch.pos);
}

fn log_segment(ap: &Autopilot, tick: u32) {
    let Some(route) = ap.route.as_ref() else {
        return;
    };
    let s = &route.segments[ap.seg];
    let p = ap.pos;
    let line = format!(
        "autopilot: seg {} {} tick={tick} at=({:.2},{:.2},{:.2}) mode={}{}",
        ap.seg,
        s.id,
        p[0],
        p[1],
        p[2],
        ap.mode,
        s.caption.as_ref().map(|c| format!(" caption=\"{c}\"")).unwrap_or_default()
    );
    diag::info!(Console, "{line}");
}

fn step_route(ap: &mut Autopilot, route: &Route, tick: u32, inputs: &mut ConsoleInputState) -> Result<(), String> {
    // expectations
    let mode = ap.mode.clone();
    let mut keep = Vec::new();
    for (seg, idx, want, deadline) in std::mem::take(&mut ap.expects) {
        if want.split('|').any(|w| w == mode) {
            ap.steps_ok += 1;
            diag::info!(Console, "autopilot: step {seg}/{idx} ok mode={want} tick={tick}");
        } else if tick > deadline {
            return Err(format!(
                "step {seg}/{idx} ({}) expected {want} got {mode} tick={tick} at=({:.2},{:.2},{:.2})",
                route.segments[seg].id, ap.pos[0], ap.pos[1], ap.pos[2]
            ));
        } else {
            keep.push((seg, idx, want, deadline));
        }
    }
    ap.expects = keep;

    loop {
        let s = &route.segments[ap.seg];
        // actions of the current segment
        for (i, a) in s.actions.iter().enumerate() {
            if ap.fired.get(i).copied().unwrap_or(true) || !a.cond.holds(ap, s.to, tick) {
                continue;
            }
            ap.fired[i] = true;
            for h in &a.release {
                inputs.release(h);
                ap.held.remove(h);
            }
            for h in &a.hold {
                if ap.held.contains(h) {
                    inputs.release(h);
                    ap.repress.push(h.clone());
                } else {
                    inputs.hold(h);
                    ap.held.insert(h.clone());
                }
            }
            for t in &a.tap {
                inputs.hold(t);
                ap.taps.push(t.clone());
            }
            if a.quickturn {
                // The game turns the view itself: move our view (and what we count as
                // already sent) with it so the spring does not fight the turn.
                inputs.hold("+quickturn");
                ap.taps.push("+quickturn".to_owned());
                ap.yaw.pos += 180.0;
                ap.sent.0 += 180.0;
                ap.target_yaw += 180.0;
            }
            if let Some(want) = &a.expect {
                ap.expects.push((ap.seg, i, want.clone(), tick + a.within_ticks));
            }
            diag::info!(
                Console,
                "autopilot: act {}/{i} {} tick={tick} mode={} at=({:.2},{:.2},{:.2}){}",
                ap.seg,
                s.id,
                ap.mode,
                ap.pos[0],
                ap.pos[1],
                ap.pos[2],
                a.caption.as_ref().map(|c| format!(" caption=\"{c}\"")).unwrap_or_default()
            );
        }
        // advance?
        let done = match &s.until {
            Some(c) => c.holds(ap, s.to, tick),
            None => {
                let (dx, dz) = (s.to[0] - ap.seg_from[0], s.to[2] - ap.seg_from[2]);
                let p = ap.lead();
                let past = (s.to[0] - p[0]) * dx + (s.to[2] - p[2]) * dz < 0.0;
                hdist(p, s.to) < s.reach || past
            }
        };
        if !done {
            if tick.wrapping_sub(ap.seg_start) > s.max_ticks {
                return Err(format!(
                    "segment {} ({}) stuck for {} ticks, mode={} at=({:.2},{:.2},{:.2})",
                    ap.seg, s.id, s.max_ticks, ap.mode, ap.pos[0], ap.pos[1], ap.pos[2]
                ));
            }
            break;
        }
        if ap.seg + 1 >= route.segments.len() {
            ap.status = Status::Done;
            return Ok(());
        }
        ap.seg_from = s.to;
        ap.seg += 1;
        ap.seg_start = tick;
        ap.fired = vec![false; route.segments[ap.seg].actions.len()];
        log_segment_inline(ap, route, tick);
    }

    // view targets for the next frames
    let s = &route.segments[ap.seg];
    let speed = (ap.vel[0].powi(2) + ap.vel[2].powi(2)).sqrt();
    let ahead = (speed * LOOKAHEAD_TIME).max(LOOKAHEAD_MIN);
    let aim = s.face.unwrap_or_else(|| pursuit_point(route, ap.seg, ap.seg_from, ap.pos, ahead));
    let (dx, dz) = (aim[0] - ap.pos[0], aim[2] - ap.pos[2]);
    if dx * dx + dz * dz > 0.01 {
        ap.target_yaw = yaw_of(dx, dz);
    }
    ap.target_pitch = LEVEL_PITCH;
    if let Some(g) = s.glance
        && hdist(ap.pos, s.to) < s.glance_dist
    {
        let h = hdist(ap.pos, g).max(0.5);
        ap.target_pitch = ((ap.pos[1] + EYE - g[1]).atan2(h).to_degrees() * 0.7).clamp(-25.0, 25.0);
    }
    Ok(())
}

fn log_segment_inline(ap: &Autopilot, route: &Route, tick: u32) {
    let s = &route.segments[ap.seg];
    let p = ap.pos;
    diag::info!(
        Console,
        "autopilot: seg {} {} tick={tick} at=({:.2},{:.2},{:.2}) mode={}{}",
        ap.seg,
        s.id,
        p[0],
        p[1],
        p[2],
        ap.mode,
        s.caption.as_ref().map(|c| format!(" caption=\"{c}\"")).unwrap_or_default()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_settles_without_overshoot() {
        let mut s = Spring { pos: 0.0, vel: 0.0 };
        let mut max: f32 = 0.0;
        for _ in 0..200 {
            s.step(90.0, YAW_OMEGA, 1.0 / 144.0);
            max = max.max(s.pos);
        }
        assert!((s.pos - 90.0).abs() < 0.5 && max <= 90.0 + 1e-3, "{} {max}", s.pos);
    }

    #[test]
    fn pursuit_walks_the_polyline() {
        let seg = |to: [f32; 3]| Segment {
            id: "s".into(),
            to,
            reach: 0.8,
            until: None,
            glance: None,
            glance_dist: 3.5,
            face: None,
            hold_line: false,
            actions: vec![],
            max_ticks: 400,
            caption: None,
        };
        let route = Route { name: String::new(), segments: vec![seg([0.0, 0.0, -10.0]), seg([10.0, 0.0, -10.0])] };
        let p = pursuit_point(&route, 0, [0.0, 0.0, 0.0], [0.0, 0.0, -8.0], 4.0);
        assert!((p[0] - 2.0).abs() < 1e-4 && (p[2] + 10.0).abs() < 1e-4, "{p:?}");
        assert!((yaw_of(0.0, -1.0)).abs() < 1e-4 && (yaw_of(-1.0, 0.0) - 90.0).abs() < 1e-4);
    }
}
