//! The movement_mec entry point: one call per usercmd, analogous to IW4
//! `Pmove`. Deterministic: same (`PlayerState`, `MecMoveState`, `UserCmd`,
//! `MecContext`, world) in, same bits out.

use movement_iw4::{CollisionBackend, update_view_angles};
use playerstate_iw4::{ENTITYNUM_NONE, PlayerState, UserCmd, buttons, pm_flags};

use crate::body::{Hull, fits, ground, reached, slide_move, step_slide_move, trace};
use crate::probes::{Ledge, Vault, ledge_ahead, lift_clear, vault_ahead, wall_along};
use crate::rootmotion::{LAND_ROLL, RootCurve, VAULT_ONTO, VAULT_ONTO_HIGH, VAULT_OVER_FAST};
use crate::state::{
    CROUCH_NEVER_MS, MecContext, MecEvent, MecMode, MecMoveResult, MecMoveState, WeaponGates,
    air_flags, script_kind as sk,
};
use crate::tuning::{METRE, MecTuning};
use crate::vec::{
    V3, ZERO, add, clampf, dot, hlen, horiz, len, mad, norm, rotate_toward, scale, sub, yaw_axes,
};

/// Longest internal substep (ms). Long commands are split so fast movement
/// does not tunnel and probes run at a steady rate.
const MAX_SUBSTEP_MS: i32 = 33;
/// Velocity pressing the body into the wall during wallrun / wallclimb.
const WALL_STICK: f32 = 30.0;
/// Minimum slide time before releasing crouch can end it (ms).
const SLIDE_MIN_MS: i32 = 250;
/// Jump driver look-direction limit (JumpVerySlow…JumpFast LookDirAngleLimit 25°):
/// the jump table speed applies when moving within this of the view.
const JUMP_LOOK_LIMIT_DEG: f32 = 25.0;
/// Speed that counts as moving (`Character.IsMoving` > 0.1 m/s).
const IS_MOVING: f32 = 0.1 * METRE;
/// Falling (`vy ≤ −0.1 m/s`) for the shorter wallrun jump-distance gate.
const FALLING_VZ: f32 = -0.1 * METRE;
/// WallClimb180 (quickturn on the wall) 58 t: time left to jump off.
const WALLCLIMB_TURN_MS: i32 = 967;
/// WallClimb180 JumpBranch window opens at 20 t: a jump pressed earlier in
/// the turn is buffered until then.
const WALLCLIMB_TURN_JUMP_MS: i32 = 333;
/// VaultOver*Seq speed target (~7 m/s): root distance scales with entry speed below it.
const VAULT_TARGET_SPEED: f32 = 7.0 * METRE;
/// GUESS: smallest rise of a springboard launched from mid-vault (the body
/// is already above the obstacle).
const SPRINGBOARD_MIN_HOP: f32 = 0.5 * METRE;
/// GUESS: clearance over the obstacle lip a ground springboard launch aims for.
const SPRINGBOARD_LIP_CLEAR: f32 = 4.0;
/// GUESS: a wall jump goes the way the camera looks when that is at least
/// this far (sine) away from the wall.
const WALLJUMP_LOOK_AWAY: f32 = 0.17;
/// GUESS: the weapon starts coming back up this long before a vault, ledge
/// climb, roll or hard landing hands control back (Catalyst's move-out
/// window), so the raise overlaps the end of the move instead of popping in
/// after it. Firing stays blocked until the move ends.
const RAISE_LEAD_MS: i32 = 250;
/// Scripted-path momentum blend: windows (s) over which the path velocity
/// eases from the entry velocity into the clip and from the clip into the
/// exit velocity (horizontal / vertical), and the largest correction (in/s).
const BLEND_H: f32 = 0.15;
const BLEND_Z: f32 = 0.1;
const BLEND_MAX: f32 = 300.0;
/// GUESS: depth of the mid-roll slow-down kept from FallingLandRoll's root
/// motion (the clip itself dips to ~25% of its entry speed; carrying that
/// read as a stall then a lurch). 0.15 = 15% at the middle of the move-out.
const ROLL_SAG: f32 = 0.15;
/// GUESS: share of a smoothstep mixed into the HangHeaveUp rise so the hang
/// between grab and heave slows the climb without stopping it.
const LEDGE_HANG_SOFTEN: f32 = 0.4;

/// Outcome of the springboard probe on the run.
enum Springboard {
    None,
    /// Obstacle ahead, not at take-off reach yet: hold the jump press.
    Ahead,
    Now(Vault),
}

#[derive(Clone, Copy)]
struct Input {
    forward: f32,
    right: f32,
    held: u32,
    pressed: u32,
    yaw: f32,
}

impl Input {
    fn held(&self, b: u32) -> bool {
        self.held & b != 0
    }
    fn pressed(&self, b: u32) -> bool {
        self.pressed & b != 0
    }
    /// Horizontal wish direction (unit or zero) and magnitude 0..=1.
    fn wish(&self) -> (V3, f32) {
        let (f, r) = yaw_axes(self.yaw);
        let d = add(scale(f, self.forward), scale(r, self.right));
        let mag = clampf(
            libm::fabsf(self.forward).max(libm::fabsf(self.right)),
            0.0,
            1.0,
        );
        (norm(d), mag)
    }
}

/// Wallrun height above entry at `t` s: constant-deceleration rise to
/// `height` at `apex`, then a quadratic fall to `end` at `total`.
#[must_use]
pub fn wallrun_arc(t: &MecTuning, s: f32) -> f32 {
    rise_fall(
        s,
        t.wallrun_apex_time,
        t.wallrun_time,
        t.wallrun_height,
        t.wallrun_end_height,
    )
}

/// Wallclimb height above entry at `s` s: 2.6 m at 1.0 s, then down by the
/// post-apex drop at the end of the move.
#[must_use]
pub fn wallclimb_arc(t: &MecTuning, s: f32) -> f32 {
    rise_fall(
        s,
        t.wallclimb_apex_time,
        t.wallclimb_time,
        t.wallclimb_height,
        t.wallclimb_height - t.wallclimb_post_apex_drop,
    )
}

fn rise_fall(s: f32, apex: f32, total: f32, height: f32, end: f32) -> f32 {
    if s <= apex {
        let u = 1.0 - s / apex.max(1.0e-4);
        height * (1.0 - u * u)
    } else {
        let u = ((s - apex) / (total - apex).max(1.0e-4)).min(1.5);
        height + (end - height) * u * u
    }
}

/// Root-motion curve that drives a scripted mode.
#[must_use]
pub fn script_curve(kind: i8) -> RootCurve {
    match kind {
        sk::VAULT_ONTO | sk::LEDGE_LOW => VAULT_ONTO,
        sk::LEDGE_HIGH => VAULT_ONTO_HIGH,
        _ => VAULT_OVER_FAST,
    }
}

/// Run one usercmd of Catalyst-style movement.
///
/// Reads/writes on `ps`: `origin`, `velocity`, `viewangles`, `delta_angles`
/// (quickturn), `ground_entity_num`, `pm_flags` CROUCH bit, `command_time`.
/// Nothing else in `ps` is touched.
pub fn mec_pmove<C: CollisionBackend>(
    ps: &mut PlayerState,
    mec: &mut MecMoveState,
    cmd: &UserCmd,
    ctx: &MecContext,
    collision: &C,
) -> MecMoveResult {
    update_view_angles(ps, cmd, ctx.view_clamp);
    let msec = ctx.msec.clamp(0, 200);
    let pressed = cmd.buttons & !mec.old_buttons;
    let mut res = MecMoveResult {
        events: [None; crate::state::MAX_EVENTS],
        mins: ZERO,
        maxs: ZERO,
        view_height: ctx.bounds.stand_view_height,
        camera_roll: 0.0,
        gates: WeaponGates {
            allow_fire: true,
            allow_ads: true,
            lowered: false,
        },
        fall_damage: 0,
        shield: 0.0,
        walking: false,
    };

    if pressed & buttons::CROUCH != 0 {
        mec.crouch_press_ms = 0;
    }
    if pressed & buttons::JUMP != 0 {
        mec.jump_press_ms = 0;
    }

    let mut m = Mover {
        ps,
        mec,
        res: &mut res,
        ctx,
        t: &ctx.tuning,
        c: collision,
    };
    if pressed & ctx.quickturn_button != 0 && ctx.tuning.unlocks.quickturn {
        m.start_quickturn();
    }

    let mut left = msec;
    let mut first = true;
    while left > 0 {
        let step = left.min(MAX_SUBSTEP_MS);
        left -= step;
        m.apply_turn(step);
        let input = Input {
            forward: f32::from(cmd.forwardmove) / 127.0,
            right: f32::from(cmd.rightmove) / 127.0,
            held: cmd.buttons,
            pressed: if first { pressed } else { 0 },
            yaw: m.ps.viewangles[1],
        };
        m.tick(&input, step);
        m.mec.crouch_press_ms = (m.mec.crouch_press_ms + step).min(CROUCH_NEVER_MS);
        m.mec.jump_press_ms = (m.mec.jump_press_ms + step).min(CROUCH_NEVER_MS);
        first = false;
    }
    m.finish();

    mec.old_buttons = cmd.buttons;
    ps.command_time = cmd.server_time;
    res
}

struct Mover<'a, C> {
    ps: &'a mut PlayerState,
    mec: &'a mut MecMoveState,
    res: &'a mut MecMoveResult,
    ctx: &'a MecContext,
    t: &'a MecTuning,
    c: &'a C,
}

impl<C: CollisionBackend> Mover<'_, C> {
    // ---------------------------------------------------------------- hulls
    fn hw(&self) -> f32 {
        self.ctx.bounds.half_width
    }

    fn stand_hull(&self) -> Hull {
        let b = self.ctx.bounds;
        Hull {
            mins: [-b.half_width, -b.half_width, 0.0],
            maxs: [b.half_width, b.half_width, b.stand_height],
            mask: b.tracemask,
        }
    }

    fn hull(&self) -> Hull {
        let b = self.ctx.bounds;
        let mut h = self.stand_hull();
        if self.mec.crouched {
            h = h.with_maxs_z(b.crouch_height);
        }
        if self.mec.coil {
            h.mins[2] = self.t.coil_lift;
        }
        h
    }

    fn set_mode(&mut self, mode: MecMode) {
        self.mec.mode = mode;
        self.mec.mode_ms = 0;
        // Callers that use `wall_side` (wallrun side, script kind) set it after.
        self.mec.wall_side = 0;
    }

    fn walk_normal(&self) -> f32 {
        self.t.min_walk_normal
    }

    /// Reach (from the axis) of the front-wall probe for wallclimb / ledge.
    fn front_reach(&self) -> f32 {
        (self.t.wallclimb_max_dist - self.hw()).max(1.0)
    }

    /// Highest ledge top above the feet that can be grabbed now: arm reach,
    /// and never ≥ 4.49 m above the jump start (CantVaultOrHeave).
    fn ledge_max_h(&self) -> f32 {
        let above_jump = self.mec.move_from[2] + self.t.ledge_max_above_jump - self.ps.origin[2];
        self.t.ledge_max_reach.min(above_jump)
    }

    fn is_new_wall(&self, n: V3) -> bool {
        let last = self.mec.last_wall_normal;
        (last[0] == 0.0 && last[1] == 0.0)
            || dot(n, last) < libm::cosf(self.t.new_wall_min_angle_deg.to_radians())
    }

    // ------------------------------------------------------------ quickturn
    fn start_quickturn(&mut self) {
        if self.mec.turn_remaining != 0.0
            || self.mec.mode.is_scripted()
            || matches!(self.mec.mode, MecMode::Roll | MecMode::HardLanding)
        {
            return;
        }
        self.mec.turn_remaining = 180.0;
        self.res.push(MecEvent::QuickTurn);
        match self.mec.mode {
            MecMode::Ground | MecMode::Slide => {
                // Turning on the spot kills momentum.
                self.ps.velocity = [0.0, 0.0, self.ps.velocity[2]];
                self.mec.momentum = 0.0;
                if self.mec.mode == MecMode::Slide {
                    self.res.push(MecEvent::SlideEnd);
                    self.set_mode(MecMode::Ground);
                }
            }
            MecMode::WallClimb => {
                self.mec.air_flags |= air_flags::TURNED_ON_WALL;
                self.mec.move_ms = self.mec.mode_ms + WALLCLIMB_TURN_MS;
            }
            MecMode::WallRun => {
                // Turn away from the wall: leave it with a light push.
                let n = self.mec.wall_normal;
                let v = mad(horiz(self.ps.velocity), n, self.t.walljump_push * 0.5);
                self.ps.velocity = [v[0], v[1], self.ps.velocity[2]];
                self.end_wallrun();
            }
            _ => {}
        }
    }

    fn apply_turn(&mut self, step: i32) {
        if self.mec.turn_remaining <= 0.0 {
            return;
        }
        // Ease in and out (smoothstep of the turn over the move-out) so the
        // view's angular speed is continuous at both ends; the progress so far
        // is recovered from what remains, so no extra state is carried.
        let p = clampf(1.0 - self.mec.turn_remaining / 180.0, 0.0, 1.0);
        let x = 0.5 - libm::sinf(libm::asinf(clampf(1.0 - 2.0 * p, -1.0, 1.0)) / 3.0);
        let x1 = (x + step as f32 / 1000.0 / self.t.quickturn_move_out.max(0.001)).min(1.0);
        let p1 = x1 * x1 * (3.0 - 2.0 * x1);
        let d = if x1 >= 1.0 - 1.0e-4 {
            self.mec.turn_remaining
        } else {
            ((p1 - p) * 180.0).clamp(0.0, self.mec.turn_remaining)
        };
        self.mec.turn_remaining -= d;
        if self.mec.turn_remaining < 1.0e-3 {
            self.mec.turn_remaining = 0.0;
        }
        self.ps.delta_angles[1] = wrap180(self.ps.delta_angles[1] + d);
        self.ps.viewangles[1] = wrap180(self.ps.viewangles[1] + d);
    }

    // ----------------------------------------------------------------- tick
    fn tick(&mut self, input: &Input, step: i32) {
        let dt = step as f32 / 1000.0;
        self.mec.mode_ms = self.mec.mode_ms.saturating_add(step);
        match self.mec.mode {
            MecMode::Ground => self.tick_ground(input, dt),
            MecMode::Air => self.tick_air(input, dt),
            MecMode::Slide => self.tick_slide(input, dt),
            MecMode::WallRun => self.tick_wallrun(input, dt),
            MecMode::WallClimb => self.tick_wallclimb(input, dt),
            MecMode::LedgeClimb | MecMode::Vault => self.tick_scripted(dt),
            MecMode::Roll => self.tick_roll(input, dt),
            MecMode::HardLanding => self.tick_hard_landing(input, dt),
        }
    }

    /// Leave the ground (walk off, jump, scripted exit): this is the jump start.
    fn enter_air(&mut self) {
        self.set_mode(MecMode::Air);
        self.mec.fall_start_z = self.ps.origin[2];
        self.mec.move_from = self.ps.origin;
        self.ps.ground_entity_num = ENTITYNUM_NONE;
        if self.mec.crouched && fits(self.c, self.ps.origin, self.stand_hull()) {
            self.mec.crouched = false;
        }
    }

    /// Ground probe; snaps down onto stairs / slopes when `snap`.
    fn find_ground(&mut self, snap: bool) -> Option<V3> {
        let hull = self.hull();
        let reach = if snap { self.t.step_height } else { 0.0 };
        let g = ground(self.c, self.ps.origin, hull, reach, self.walk_normal())?;
        if g.origin[2] < self.ps.origin[2] {
            self.ps.origin = g.origin;
        }
        self.ps.ground_entity_num = g.entity;
        Some(g.normal)
    }

    // --------------------------------------------------------------- ground
    fn tick_ground(&mut self, input: &Input, dt: f32) {
        let Some(normal) = self.find_ground(self.ps.velocity[2] <= 1.0) else {
            if self.jump_buffered() {
                // A press held for a springboard that the edge came before:
                // jump off the edge instead of falling.
                let vel = horiz(self.ps.velocity);
                let (fwd, _) = yaw_axes(input.yaw);
                self.ground_jump(hlen(vel), fwd);
            } else {
                self.enter_air();
            }
            self.tick_air(input, dt);
            return;
        };
        let t = *self.t;
        let mut vel = horiz(self.ps.velocity);
        let speed = hlen(vel);
        let (fwd, _) = yaw_axes(input.yaw);
        let turning = self.mec.turn_remaining > 0.0;

        if input.pressed(buttons::CROUCH) && speed >= t.slide_min_speed && !self.mec.crouched {
            self.mec.crouched = true;
            self.set_mode(MecMode::Slide);
            self.res.push(MecEvent::SlideStart);
            self.tick_slide(input, dt);
            return;
        }
        self.mec.crouched = input.held(buttons::CROUCH)
            || (self.mec.crouched && !fits(self.c, self.ps.origin, self.stand_hull()));

        // Jump (fresh or buffered): a springboard obstacle reached within the
        // sighting window holds the press until the take-off point; anything
        // else jumps now.
        if self.jump_buffered() {
            let sb = if turning || self.mec.crouched {
                Springboard::None
            } else {
                self.springboard_probe(speed, fwd)
            };
            let used = match sb {
                Springboard::Now(v) => {
                    let dir = norm(horiz(self.ps.velocity));
                    let ground_z = self.ps.origin[2];
                    self.start_springboard(dir, speed, v.top_z, Some(v.wall_dist), ground_z);
                    true
                }
                Springboard::Ahead => false,
                Springboard::None => self.try_ground_jump(input, speed, fwd),
            };
            if used {
                if self.mec.mode == MecMode::Air {
                    self.tick_air(input, dt);
                }
                return;
            }
        }

        // Vault: stick forward, aim forward, obstacle within the AllowedToVault reach.
        if !turning
            && input.forward > 0.0
            && !self.mec.crouched
            && (speed < IS_MOVING || dot(norm(vel), fwd) > 0.5)
            && let Some(v) = vault_ahead(
                self.c,
                self.ps.origin,
                fwd,
                speed,
                None,
                self.stand_hull(),
                self.hw(),
                self.t,
            )
        {
            self.start_vault(&v, fwd, speed);
            return;
        }

        // StandRunTurn180 holds the walking driver until its move-out.
        let (wish, mag) = if turning { (ZERO, 0.0) } else { input.wish() };
        let running = input.forward > 0.0 && !self.mec.crouched && !turning;
        let curve_t = self.run_curve_time(speed) + dt;
        let target = if self.mec.crouched {
            t.crouch_speed * mag
        } else if running {
            t.run_speed_at(curve_t) * mag
        } else {
            t.walk_speed * mag
        } * self.ctx.speed_scale;
        if mag > 0.0 {
            if speed > 1.0 && dot(norm(vel), wish) > 0.0 {
                vel = rotate_toward(vel, wish, t.turn_rate_deg.to_radians() * dt);
            }
            let diff = sub(scale(wish, target), vel);
            let dl = len(diff);
            let rate = if target < speed {
                t.above_max_decel
            } else {
                t.ground_accel
            };
            let max = rate * dt;
            vel = if dl > max {
                mad(vel, diff, max / dl)
            } else {
                add(vel, diff)
            };
        } else if speed > 0.0 {
            let ns = (speed - t.ground_decel * dt).max(0.0);
            vel = scale(vel, ns / speed);
        }

        let out = step_slide_move(
            self.c,
            self.ps.origin,
            [vel[0], vel[1], 0.0],
            dt,
            self.hull(),
            Some(normal),
            t.step_height,
            self.walk_normal(),
        );
        self.ps.origin = out.origin;
        self.ps.velocity = [out.velocity[0], out.velocity[1], 0.0];

        let s = hlen(self.ps.velocity);
        if running && s + 1.0 >= t.run_start_speed() * self.ctx.speed_scale.min(1.0) {
            self.mec.momentum = (curve_t / t.run_curve_time).min(1.0);
        }
        self.cap_momentum();

        if self.find_ground(true).is_none() {
            self.enter_air();
        }
    }

    /// Position on the sprint curve (s): the stored momentum, never more than
    /// the actual speed supports.
    fn run_curve_time(&self, speed: f32) -> f32 {
        let t = self.t;
        if speed < t.run_start_speed() * 0.5 {
            return 0.0;
        }
        (self.mec.momentum * t.run_curve_time).min(t.run_time_for(speed))
    }

    /// Momentum can never exceed what the actual speed supports: hitting a
    /// wall, stopping or turning hard bleeds it.
    fn cap_momentum(&mut self) {
        let s = hlen(self.ps.velocity);
        self.mec.momentum = clampf(self.run_curve_time(s) / self.t.run_curve_time, 0.0, 1.0);
    }

    /// A jump press not yet used by a move, within the springboard sighting window.
    fn jump_buffered(&self) -> bool {
        self.mec.jump_press_ms as f32 <= self.t.springboard_sighting_time * 1000.0
    }

    fn consume_jump(&mut self) {
        self.mec.jump_press_ms = CROUCH_NEVER_MS;
    }

    /// Springboard.IsAllowed on the run: moving forward at springboard speed
    /// at an obstacle whose top is 0.9–1.5 m above the feet. `Now` once the
    /// face is within the take-off reach (the assist distance, or the vault
    /// reach when that is longer, so the vault never pre-empts it); `Ahead`
    /// while it will be reached before the buffered press runs out.
    fn springboard_probe(&self, speed: f32, fwd: V3) -> Springboard {
        let t = self.t;
        let vel = horiz(self.ps.velocity);
        if speed < t.springboard_min_speed || dot(norm(vel), fwd) <= 0.7 {
            return Springboard::None;
        }
        let now = t
            .springboard_assist_dist
            .max(t.vault_reach(t.springboard_obstacle_max, speed));
        let left = (t.springboard_sighting_time - self.mec.jump_press_ms as f32 / 1000.0).max(0.0);
        let reach = now + speed * left;
        let o = self.ps.origin;
        let Some(v) = vault_ahead(
            self.c,
            o,
            norm(vel),
            speed,
            Some(reach),
            self.stand_hull(),
            self.hw(),
            t,
        ) else {
            return Springboard::None;
        };
        let h = v.top_z - o[2];
        if h < t.springboard_obstacle_min || h > t.springboard_obstacle_max {
            Springboard::None
        } else if v.wall_dist <= now {
            Springboard::Now(v)
        } else {
            Springboard::Ahead
        }
    }

    /// Springboard driver: forward 5.5–7.0 m/s and an apex 1.7–2.8 m above
    /// the jump start (`ground_z`) by entry speed. From the ground (`face` =
    /// distance from the axis to the obstacle face) the launch also clears
    /// the obstacle's lip, as the plant on the obstacle does in the clip.
    fn start_springboard(
        &mut self,
        dir: V3,
        speed: f32,
        top_z: f32,
        face: Option<f32>,
        ground_z: f32,
    ) {
        let t = *self.t;
        let f = clampf(
            (speed - t.springboard_min_speed) / (t.springboard_max_speed - t.springboard_min_speed),
            0.0,
            1.0,
        );
        let jh =
            t.springboard_min_height + (t.springboard_max_height - t.springboard_min_height) * f;
        let hs = clampf(speed, t.springboard_min_speed, t.springboard_max_speed);
        let z = self.ps.origin[2];
        let mut vz = t.jump_velocity((ground_z + jh - z).max(SPRINGBOARD_MIN_HOP));
        if let Some(d) = face {
            let tt = (d - self.hw()).max(1.0) / hs;
            let need = (top_z + SPRINGBOARD_LIP_CLEAR - z + 0.5 * t.gravity * tt * tt) / tt;
            let cap = t.jump_velocity(t.springboard_max_height + t.springboard_obstacle_max);
            vz = vz.max(need.min(cap));
        }
        self.enter_air();
        self.mec.air_flags = 0;
        let v = scale(dir, hs);
        self.ps.velocity = [v[0], v[1], vz];
        self.consume_jump();
        self.res.push(MecEvent::Springboard);
    }

    /// Jump pressed on the ground (no springboard): ledge > wallclimb > jump.
    fn try_ground_jump(&mut self, _input: &Input, speed: f32, fwd: V3) -> bool {
        let t = *self.t;
        let o = self.ps.origin;
        self.consume_jump();

        if let Some(w) = wall_along(
            self.c,
            o,
            fwd,
            self.hw(),
            self.front_reach(),
            self.hull().mask,
        ) && dot(fwd, scale(w.normal, -1.0))
            >= libm::cosf(t.wallclimb_max_angle_deg.to_radians())
            && w.dist <= t.wallclimb_max_dist
        {
            let into = scale(w.normal, -1.0);
            self.mec.move_from = o;
            if let Some(l) = ledge_ahead(
                self.c,
                o,
                into,
                w.dist,
                t.step_height + 1.0,
                self.ledge_max_h(),
                self.stand_hull(),
                self.hw(),
            ) {
                self.start_ledge(&l, into, speed);
                return true;
            }
            if self.wallclimb_allowed(w.normal) {
                self.mec.fall_start_z = o[2];
                self.start_wallclimb(w.normal);
                return true;
            }
        }

        self.ground_jump(speed, fwd);
        true
    }

    /// JumpDatabase: forward speed and height by entry speed.
    fn ground_jump(&mut self, speed: f32, fwd: V3) {
        let t = *self.t;
        let vel = horiz(self.ps.velocity);
        self.consume_jump();
        let row = t.jump_row(speed);
        let dir = if speed > 1.0 { norm(vel) } else { fwd };
        let look_ok = dot(dir, fwd) >= libm::cosf(JUMP_LOOK_LIMIT_DEG.to_radians());
        let hs = if look_ok {
            row.forward_speed * self.ctx.speed_scale.min(1.0)
        } else {
            speed.min(row.forward_speed.max(t.walk_speed))
        };
        self.enter_air();
        let v = scale(dir, hs);
        self.ps.velocity = [v[0], v[1], t.jump_velocity(row.height)];
        self.res.push(MecEvent::Jump);
    }

    // ------------------------------------------------------------------ air
    fn tick_air(&mut self, input: &Input, dt: f32) {
        let t = *self.t;
        self.mec.crouched = false;

        // Coil: crouch while still rising tucks the legs (CoilDriver; not once falling).
        if input.held(buttons::CROUCH)
            && t.unlocks.coil
            && !self.mec.coil
            && self.ps.velocity[2] >= 0.0
        {
            self.mec.coil = true;
            self.res.push(MecEvent::CoilStart);
        } else if !input.held(buttons::CROUCH)
            && self.mec.coil
            && fits(self.c, self.ps.origin, self.stand_hull())
        {
            self.mec.coil = false;
        }

        if self.try_wall_moves(input) {
            return;
        }

        let (wish, mag) = input.wish();
        let mut vh = horiz(self.ps.velocity);
        if mag > 0.0 {
            let before = hlen(vh).max(t.walk_speed);
            vh = mad(vh, wish, t.air_accel * mag * dt);
            let after = hlen(vh);
            if after > before {
                vh = scale(vh, before / after);
            }
        }
        let vz0 = self.ps.velocity[2];
        let vz1 = vz0 - t.gravity * dt;
        let avg = 0.5 * (vz0 + vz1);
        let before = self.ps.origin;
        let out = slide_move(
            self.c,
            self.ps.origin,
            [vh[0], vh[1], avg],
            dt,
            self.hull(),
            None,
            self.walk_normal(),
        );
        self.ps.origin = out.origin;
        self.ps.velocity = [
            out.velocity[0],
            out.velocity[1],
            out.velocity[2] + (vz1 - avg),
        ];
        if out.wall.is_some() && vz1 <= 0.0 && self.air_step(before, vh, dt) {
            self.ps.velocity = [vh[0], vh[1], 0.0];
            self.land(input);
            return;
        }
        if self.ps.origin[2] > self.mec.fall_start_z {
            self.mec.fall_start_z = self.ps.origin[2];
        }
        if self.ps.velocity[2] <= 0.0 && self.find_ground(false).is_some() {
            self.land(input);
        }
    }

    /// IsWallClimbAllowed minus the geometry: wallclimbs < 1, wall moves < 2,
    /// a new wall.
    fn wallclimb_allowed(&self, n: V3) -> bool {
        let f = self.mec.air_flags;
        air_flags::wallclimbs(f) < self.t.wallclimbs_per_air
            && air_flags::wall_moves(f) < self.t.wall_moves_per_air
            && self.is_new_wall(n)
    }

    /// Ledge grab, wallclimb and wallrun entry from the air.
    fn try_wall_moves(&mut self, input: &Input) -> bool {
        let t = *self.t;
        let o = self.ps.origin;
        let (fwd, _) = yaw_axes(input.yaw);
        let mask = self.hull().mask;
        let jump = input.held(buttons::JUMP);
        let vz = self.ps.velocity[2];

        if (jump || input.forward > 0.0)
            && let Some(w) = wall_along(self.c, o, fwd, self.hw(), self.front_reach(), mask)
            && dot(fwd, scale(w.normal, -1.0)) >= libm::cosf(t.wallclimb_max_angle_deg.to_radians())
            && w.dist <= t.wallclimb_max_dist
        {
            let into = scale(w.normal, -1.0);
            let max_h = self.ledge_max_h();
            if max_h > t.step_height + 1.0
                && let Some(l) = ledge_ahead(
                    self.c,
                    o,
                    into,
                    w.dist,
                    t.step_height + 1.0,
                    max_h,
                    self.stand_hull(),
                    self.hw(),
                )
            {
                let s = hlen(self.ps.velocity);
                self.mec.coil = false;
                self.start_ledge(&l, into, s);
                return true;
            }
            if jump && vz > -t.wallclimb_max_fall_speed && self.wallclimb_allowed(w.normal) {
                self.start_wallclimb(w.normal);
                return true;
            }
        }

        let f = self.mec.air_flags;
        let vh = horiz(self.ps.velocity);
        let speed = hlen(vh);
        if jump
            && speed > IS_MOVING
            && vz > -t.wallrun_max_fall_speed
            && air_flags::wallruns(f) < t.wallruns_per_air
            && air_flags::wall_moves(f) < t.wall_moves_per_air
        {
            let d = norm(vh);
            let right = [d[1], -d[0], 0.0];
            let max_into = libm::sinf(t.wallrun_max_entry_angle_deg.to_radians());
            let max_jump = if vz <= FALLING_VZ {
                t.wallrun_max_jump_dist_falling
            } else {
                t.wallrun_max_jump_dist
            };
            // The driver aligns the body onto the wall over AlignTime: a wall
            // that the current speed towards it closes within that time counts.
            let probe = t.wallrun_attach_dist + speed * t.wallrun_align_time;
            for (side, dir) in [(-1_i8, scale(right, -1.0)), (1_i8, right)] {
                let Some(w) = wall_along(self.c, o, dir, self.hw(), probe, mask) else {
                    continue;
                };
                let inward = scale(w.normal, -1.0);
                let into = dot(d, inward);
                let approach = dot(vh, inward).max(0.0);
                let gap = (w.dist * dot(dir, inward) - self.hw()).max(0.0);
                let hit = mad(o, dir, w.dist);
                let from_jump = hlen(sub(hit, self.mec.move_from));
                if into >= -0.2
                    && into <= max_into
                    && gap <= t.wallrun_attach_dist + approach * t.wallrun_align_time
                    && self.is_new_wall(w.normal)
                    && from_jump < max_jump
                    && self.wall_runs_on(o, d, dir, probe, w.normal)
                {
                    self.start_wallrun(w.normal, side, gap);
                    return true;
                }
            }
        }
        false
    }

    /// The wall found beside the body is a face that runs along the travel:
    /// the same plane is found a half width further on. Rejects the end of a
    /// wall, whose corner reads (capsule against its edge) as a slanted face.
    fn wall_runs_on(&self, o: V3, d: V3, dir: V3, probe: f32, n: V3) -> bool {
        let ahead = mad(o, d, self.hw());
        wall_along(self.c, ahead, dir, self.hw(), probe, self.hull().mask)
            .is_some_and(|w| dot(w.normal, n) > 0.98)
    }

    /// Falling against a low face (a curb past a gap, a step taken late):
    /// step up onto it as IW4 air movement does instead of sliding down it.
    fn air_step(&mut self, before: V3, vh: V3, dt: f32) -> bool {
        let hull = self.hull();
        let up_end = add(before, [0.0, 0.0, self.t.step_height]);
        let up = trace(self.c, before, up_end, hull);
        if up.allsolid != 0 || up.startsolid != 0 {
            return false;
        }
        let raised = reached(&up, up_end);
        let ahead = mad(raised, [vh[0], vh[1], 0.0], dt);
        let across = trace(self.c, raised, ahead, hull);
        if across.startsolid != 0 {
            return false;
        }
        let moved = reached(&across, ahead);
        if hlen(sub(moved, raised)) < 1.0 {
            return false;
        }
        let Some(g) = ground(self.c, moved, hull, self.t.step_height, self.walk_normal()) else {
            return false;
        };
        if g.origin[2] < before[2] {
            return false;
        }
        self.ps.origin = g.origin;
        self.ps.ground_entity_num = g.entity;
        true
    }

    /// LandConduit: death > roll (crouch held at touchdown, drop > 1 m) >
    /// heavy landing tiers by drop > plain landing / slide. The drop is
    /// measured from the jump start.
    fn land(&mut self, input: &Input) {
        let t = *self.t;
        if self.mec.coil {
            let lifted = lift_clear(self.c, self.ps.origin, t.coil_lift, self.hull());
            if fits(self.c, lifted, self.stand_hull()) {
                self.ps.origin = lifted;
                self.mec.coil = false;
            } else {
                self.mec.coil = false;
                self.mec.crouched = true;
            }
        }
        let fall = (self.mec.fall_start_z - self.ps.origin[2]).max(0.0);
        let drop = self.mec.move_from[2] - self.ps.origin[2];
        self.ps.velocity[2] = 0.0;
        // A press left over from the air is not buffered into the landing.
        self.consume_jump();
        self.mec.air_flags = 0;
        self.mec.last_wall_normal = ZERO;

        if drop > t.lethal_fall_height {
            self.res.fall_damage = 1000;
            self.hard_land(drop, sk::DEATH);
        } else if input.held(buttons::CROUCH) && drop > t.roll_min_drop {
            self.start_roll(input, drop);
        } else if drop > t.fail_height {
            let f = (drop - t.fail_height) / (t.lethal_fall_height - t.fail_height);
            self.res.fall_damage = lerp1(t.fail_damage, f) as i32;
            self.hard_land(drop, sk::FAIL);
        } else if drop > t.fail_medium_height {
            let f = (drop - t.fail_medium_height) / (t.fail_height - t.fail_medium_height);
            self.res.fall_damage = lerp1(t.fail_medium_damage, f) as i32;
            self.hard_land(drop, sk::FAIL_MEDIUM);
        } else if drop > t.stumble_height {
            self.hard_land(drop, sk::STUMBLE);
        } else {
            self.res.push(MecEvent::Land { fall_height: fall });
            if input.held(buttons::CROUCH) && hlen(self.ps.velocity) >= t.slide_min_speed {
                self.mec.crouched = true;
                self.set_mode(MecMode::Slide);
                self.res.push(MecEvent::SlideStart);
            } else {
                self.set_mode(MecMode::Ground);
            }
        }
    }

    fn hard_land(&mut self, h: f32, kind: i8) {
        let t = self.t;
        let control = match kind {
            sk::STUMBLE => t.stumble_time,
            sk::FAIL_MEDIUM => t.fail_medium_move_out,
            _ => t.fail_move_out,
        };
        if kind == sk::STUMBLE {
            let v = horiz(self.ps.velocity);
            let s = hlen(v);
            if s > t.stumble_speed {
                self.ps.velocity = scale(v, t.stumble_speed / s);
            }
        } else {
            self.ps.velocity = ZERO;
        }
        self.mec.momentum = 0.0;
        self.set_mode(MecMode::HardLanding);
        self.mec.wall_side = kind;
        self.mec.move_ms = (control * 1000.0) as i32;
        self.res.push(MecEvent::HardLanding { fall_height: h });
    }

    // -------------------------------------------------------------- wallrun
    fn start_wallrun(&mut self, n: V3, side: i8, gap: f32) {
        let t = self.t;
        let vh = horiz(self.ps.velocity);
        let along = norm(sub(vh, scale(n, dot(vh, n))));
        // Length 4–10 m over the wallrun: along-wall speed clamp.
        let speed = clampf(
            hlen(vh),
            t.wallrun_min_length / t.wallrun_time,
            t.wallrun_max_length / t.wallrun_time,
        );
        let first = air_flags::wallruns(self.mec.air_flags) == 0;
        let time = if first {
            t.wallrun_time
        } else {
            t.wallrun_second_time
        };
        let vz = wallrun_arc(t, 0.02) / 0.02;
        self.ps.velocity = [along[0] * speed, along[1] * speed, vz];
        self.mec.wall_normal = n;
        self.mec.last_wall_normal = n;
        self.mec.air_flags |= if first {
            air_flags::USED_WALLRUN
        } else {
            air_flags::USED_WALLRUN_2
        };
        self.mec.air_flags = air_flags::add_wall_move(self.mec.air_flags);
        self.mec.coil = false;
        self.set_mode(MecMode::WallRun);
        self.mec.wall_side = side;
        self.mec.move_mid = [gap, 0.0, self.ps.origin[2]];
        self.mec.move_ms = (time * 1000.0) as i32;
        self.consume_jump();
        self.res.push(MecEvent::WallRunStart { side });
    }

    fn end_wallrun(&mut self) {
        self.mec.wall_side = 0;
        self.set_mode(MecMode::Air);
        self.res.push(MecEvent::WallRunEnd);
    }

    /// A wall jump is a new jump: new jump start, new fall reference.
    fn wall_jump_start(&mut self) {
        self.mec.move_from = self.ps.origin;
        self.mec.fall_start_z = self.ps.origin[2];
        self.res.push(MecEvent::WallJump);
    }

    fn tick_wallrun(&mut self, input: &Input, dt: f32) {
        let t = *self.t;
        let n = self.mec.wall_normal;
        let vh = horiz(self.ps.velocity);
        let along = sub(vh, scale(n, dot(vh, n)));
        let speed = hlen(along);
        let dir = norm(along);
        let s1 = self.mec.mode_ms as f32 / 1000.0;
        let s0 = (s1 - dt).max(0.0);

        if input.pressed(buttons::JUMP) {
            // WallrunJumpDriver: JumpHeight 1.1 m, ForwardSpeed 0 with a 180°
            // look-direction limit: the jump keeps the wallrun speed and goes
            // where the camera looks when that is away from the wall; it
            // always leaves the wall at `walljump_push` or more.
            let (look, _) = yaw_axes(input.yaw);
            let away = dot(look, n);
            let mut v = if away >= WALLJUMP_LOOK_AWAY {
                scale(look, speed.max(t.walljump_push))
            } else {
                scale(dir, speed)
            };
            let off = dot(v, n);
            if off < t.walljump_push {
                v = mad(v, n, t.walljump_push - off);
            }
            self.ps.velocity = [v[0], v[1], t.jump_velocity(t.jump_height)];
            self.consume_jump();
            self.end_wallrun();
            self.wall_jump_start();
            self.tick_air(input, dt);
            return;
        }
        // Align: the entry gap closes over AlignTime, then the usual reach.
        let align_ms = (t.wallrun_align_time * 1000.0) as i32;
        let gap0 = self.mec.move_mid[0].max(0.0);
        let aligning = self.mec.mode_ms <= align_ms + 1;
        let reach = if aligning {
            t.wallrun_attach_dist + gap0
        } else {
            t.wallrun_attach_dist
        };
        let wall = wall_along(
            self.c,
            self.ps.origin,
            scale(n, -1.0),
            self.hw(),
            reach,
            self.hull().mask,
        )
        .filter(|w| dot(w.normal, n) > 0.9);
        let on_wall = wall.is_some();
        let expired = self.mec.mode_ms >= self.mec.move_ms;
        if input.pressed(buttons::CROUCH) || !on_wall || expired || speed < 1.0 {
            let vz = (wallrun_arc(&t, s1) - wallrun_arc(&t, s0)) / dt.max(1.0e-4);
            let v = mad(scale(dir, speed), n, WALL_STICK);
            self.ps.velocity = [v[0], v[1], vz];
            self.end_wallrun();
            self.tick_air(input, dt);
            return;
        }

        // Scripted vertical arc: +1.2 m at 0.533 s, then down.
        let want_z = self.mec.move_mid[2] + wallrun_arc(&t, s1);
        let vz_move = (want_z - self.ps.origin[2]) / dt.max(1.0e-4);
        // Pressed into the wall; while aligning, fast enough to close the
        // remaining gap by the end of AlignTime.
        let stick = match wall {
            Some(w) if aligning => {
                let left = ((align_ms - self.mec.mode_ms).max(0) as f32 / 1000.0 + dt).max(dt);
                WALL_STICK.max((w.dist - self.hw()).max(0.0) / left)
            }
            _ => WALL_STICK,
        };
        let v = mad(scale(dir, speed), n, -stick);
        let out = slide_move(
            self.c,
            self.ps.origin,
            [v[0], v[1], vz_move],
            dt,
            self.hull(),
            None,
            self.walk_normal(),
        );
        self.ps.origin = out.origin;
        let vz = (wallrun_arc(&t, s1) - wallrun_arc(&t, s0)) / dt.max(1.0e-4);
        // Keep the along-wall speed; the stick component is consumed by the wall.
        self.ps.velocity = [dir[0] * speed, dir[1] * speed, vz];
        if self.ps.origin[2] > self.mec.fall_start_z {
            self.mec.fall_start_z = self.ps.origin[2];
        }
        if vz <= 0.0 && self.find_ground(false).is_some() {
            self.res.push(MecEvent::WallRunEnd);
            self.land(input);
        }
    }

    // ------------------------------------------------------------ wallclimb
    fn start_wallclimb(&mut self, n: V3) {
        let t = self.t;
        self.mec.wall_normal = n;
        self.mec.last_wall_normal = n;
        self.mec.air_flags |= if air_flags::wallclimbs(self.mec.air_flags) == 0 {
            air_flags::USED_WALLCLIMB
        } else {
            air_flags::USED_WALLCLIMB_2
        };
        self.mec.air_flags &= !air_flags::TURNED_ON_WALL;
        self.mec.air_flags = air_flags::add_wall_move(self.mec.air_flags);
        self.mec.coil = false;
        self.mec.crouched = false;
        self.ps.velocity = [0.0, 0.0, wallclimb_arc(t, 0.02) / 0.02];
        self.ps.ground_entity_num = ENTITYNUM_NONE;
        self.set_mode(MecMode::WallClimb);
        self.mec.move_mid = [0.0, 0.0, self.ps.origin[2]];
        self.mec.move_ms = (t.wallclimb_time * 1000.0) as i32;
        self.res.push(MecEvent::WallClimbStart);
    }

    fn end_wallclimb(&mut self) {
        self.set_mode(MecMode::Air);
        self.res.push(MecEvent::WallClimbEnd);
    }

    fn tick_wallclimb(&mut self, input: &Input, dt: f32) {
        let t = *self.t;
        let n = self.mec.wall_normal;
        let into = scale(n, -1.0);
        let turned = self.mec.air_flags & air_flags::TURNED_ON_WALL != 0;
        let s1 = self.mec.mode_ms as f32 / 1000.0;
        let s0 = (s1 - dt).max(0.0);

        // WallClimb180 jump (WallclimbJump180Driver): JumpHeight 1.1 m,
        // ForwardSpeed 7.2 m/s, look-direction limit 180°: off the wall the way
        // the camera faces once the turn completes (the window opens mid-turn),
        // straight out when that still faces the wall. A press since the turn
        // began is buffered until the window opens.
        let since_turn = self.mec.mode_ms - (self.mec.move_ms - WALLCLIMB_TURN_MS);
        if turned
            && since_turn >= WALLCLIMB_TURN_JUMP_MS
            && (input.pressed(buttons::JUMP) || self.mec.jump_press_ms < since_turn)
        {
            let (look, _) = yaw_axes(input.yaw + self.mec.turn_remaining);
            let dir = if dot(look, n) >= WALLJUMP_LOOK_AWAY {
                look
            } else {
                n
            };
            let v = scale(dir, t.wallclimb_turn_jump_speed);
            self.ps.velocity = [v[0], v[1], t.jump_velocity(t.jump_height)];
            self.consume_jump();
            self.mec.air_flags &= !air_flags::TURNED_ON_WALL;
            self.end_wallclimb();
            self.wall_jump_start();
            self.tick_air(input, dt);
            return;
        }
        let wall = wall_along(
            self.c,
            self.ps.origin,
            into,
            self.hw(),
            self.front_reach(),
            self.hull().mask,
        );
        let max_h = self.ledge_max_h();
        if !turned
            && max_h > t.step_height + 1.0
            && let Some(w) = wall
            && let Some(l) = ledge_ahead(
                self.c,
                self.ps.origin,
                into,
                w.dist,
                t.step_height + 1.0,
                max_h,
                self.stand_hull(),
                self.hw(),
            )
        {
            self.start_ledge(&l, into, 0.0);
            return;
        }
        let expired = self.mec.mode_ms >= self.mec.move_ms;
        if input.pressed(buttons::CROUCH) || wall.is_none() || expired {
            if !turned {
                let vz = (wallclimb_arc(&t, s1) - wallclimb_arc(&t, s0)) / dt.max(1.0e-4);
                self.ps.velocity = [0.0, 0.0, vz.min(0.0)];
            }
            self.mec.air_flags &= !air_flags::TURNED_ON_WALL;
            self.end_wallclimb();
            self.tick_air(input, dt);
            return;
        }
        let (vz_move, vz) = if turned {
            // WallClimb180: hang on the wall, sliding down slowly.
            let vz0 = self.ps.velocity[2].min(0.0);
            let vz1 = vz0 - t.gravity * 0.25 * dt;
            (0.5 * (vz0 + vz1), vz1)
        } else {
            let want_z = self.mec.move_mid[2] + wallclimb_arc(&t, s1);
            (
                (want_z - self.ps.origin[2]) / dt.max(1.0e-4),
                (wallclimb_arc(&t, s1) - wallclimb_arc(&t, s0)) / dt.max(1.0e-4),
            )
        };
        let v = scale(into, WALL_STICK);
        let out = slide_move(
            self.c,
            self.ps.origin,
            [v[0], v[1], vz_move],
            dt,
            self.hull(),
            None,
            self.walk_normal(),
        );
        self.ps.origin = out.origin;
        let blocked = out.velocity[2] < vz_move - 1.0 && vz_move > 0.0;
        self.ps.velocity = [0.0, 0.0, if blocked { 0.0 } else { vz }];
        if self.ps.origin[2] > self.mec.fall_start_z {
            self.mec.fall_start_z = self.ps.origin[2];
        }
        if self.ps.velocity[2] <= 0.0 && self.find_ground(false).is_some() {
            self.res.push(MecEvent::WallClimbEnd);
            self.land(input);
        }
    }

    // ------------------------------------------------------ ledge / vault
    /// Climb onto a ledge. A top no higher above the jump start than a vault
    /// (1.7 m) uses the VaultOnto curve (0.5 s); higher ones heave up along the
    /// VaultOntoHigh curve with the HangHeaveUp move-out (1.0 s).
    fn start_ledge(&mut self, l: &Ledge, dir: V3, speed: f32) {
        let t = self.t;
        let low = l.top_z - self.mec.move_from[2] <= t.vault_max_height;
        let (kind, time) = if low {
            (sk::LEDGE_LOW, t.ledge_climb_low_time)
        } else {
            (sk::LEDGE_HIGH, t.ledge_climb_time)
        };
        // Hand back at least a walk: the heave ends stepping onto the top.
        let exit = clampf(speed, t.walk_speed.min(t.run_start_speed()), t.run_start_speed());
        self.mec.move_from = self.ps.origin;
        self.mec.move_mid = l.mid;
        self.mec.move_to = l.target;
        self.mec.move_ms = (time * 1000.0) as i32;
        self.mec.exit_velocity = scale(dir, exit);
        self.mec.wall_normal = self.ps.velocity;
        self.mec.coil = false;
        self.mec.crouched = false;
        self.set_mode(MecMode::LedgeClimb);
        self.mec.wall_side = kind;
        self.res.push(MecEvent::LedgeClimbStart);
    }

    fn start_vault(&mut self, v: &Vault, dir: V3, speed: f32) {
        let t = self.t;
        let o = self.ps.origin;
        let (time, root) = match v.kind {
            sk::VAULT_OVER => (t.vault_short_time, t.vault_short_dist),
            sk::VAULT_OVER_LONG => (t.vault_long_time, t.vault_long_dist),
            _ => (t.ledge_climb_low_time, 0.0),
        };
        let mut to = v.target;
        if !v.onto {
            // Root motion length (scaled to the entry speed) unless the
            // obstacle needs more.
            let geom = hlen(sub(v.target, o));
            let want = root * clampf(speed / VAULT_TARGET_SPEED, 0.0, 1.0);
            if want > geom {
                let p = mad(o, dir, want);
                to = [p[0], p[1], v.target[2]];
            }
        }
        let exit = if v.onto {
            speed.min(t.vault_exit_max_speed)
        } else {
            clampf(speed, t.vault_exit_min_speed, t.vault_exit_max_speed)
        };
        self.mec.move_from = o;
        self.mec.move_mid = v.mid;
        self.mec.move_to = to;
        self.mec.move_ms = (time * 1000.0) as i32;
        self.mec.exit_velocity = scale(dir, exit);
        self.mec.wall_normal = self.ps.velocity;
        self.set_mode(MecMode::Vault);
        self.mec.wall_side = v.kind;
        self.res.push(MecEvent::VaultStart { onto: v.onto });
    }

    /// Point on the scripted path at fraction `f`, from the clip's root motion:
    /// rise (normalised to the clip's peak) times the rise to the lip, forward
    /// (normalised to the clip's travel) times the horizontal path.
    fn script_point(&self, f: f32) -> V3 {
        let from = self.mec.move_from;
        let to = self.mec.move_to;
        let kind = self.mec.wall_side;
        let curve = script_curve(kind);
        let fwd = curve.normalized(f).1;
        let over = matches!(kind, sk::VAULT_OVER | sk::VAULT_OVER_LONG);
        let z = self.script_z(f);
        let h = horiz(sub(to, from));
        let fwd = if over {
            fwd
        } else {
            // Stay behind the face while below the lip; once over it, spread
            // the rest of the way across the remaining clip (rather than
            // catching up the held-back distance in one tick).
            let (fc, base) = self.lip_clear();
            if f < fc {
                fwd.min(base)
            } else {
                let at = curve.normalized(fc).1;
                if at >= 1.0 - 1.0e-4 {
                    1.0
                } else {
                    let b = at.min(base);
                    b + (1.0 - b) * ((fwd - at) / (1.0 - at)).max(0.0)
                }
            }
        };
        [from[0] + h[0] * fwd, from[1] + h[1] * fwd, z]
    }

    /// Height of the scripted path at fraction `f`.
    fn script_z(&self, f: f32) -> f32 {
        let from = self.mec.move_from;
        let mid = self.mec.move_mid;
        let to = self.mec.move_to;
        let kind = self.mec.wall_side;
        let curve = script_curve(kind);
        let up = curve.normalized(f).0;
        match kind {
            sk::VAULT_OVER | sk::VAULT_OVER_LONG => from[2] + (mid[2] - from[2]) * up,
            sk::VAULT_ONTO => {
                // Rise to the probed clearance over the lip, then settle onto
                // the top over the clip's hold after the peak.
                let peak = curve.peak_fraction();
                let top = if f > peak && peak < 1.0 {
                    mid[2] + (to[2] - mid[2]) * (f - peak) / (1.0 - peak)
                } else {
                    mid[2]
                };
                from[2] + (top - from[2]) * up
            }
            sk::LEDGE_HIGH => {
                // HangHeaveUp: the clip hangs (almost no rise for ~0.1 s)
                // between the grab and the heave, which reads as a stall in
                // first person; keep most of its shape, never quite stop.
                let peak = curve.peak_fraction().max(1.0e-3);
                let x = (f / peak).clamp(0.0, 1.0);
                let up = (1.0 - LEDGE_HANG_SOFTEN) * up
                    + LEDGE_HANG_SOFTEN * x * x * (3.0 - 2.0 * x);
                from[2] + (to[2] - from[2]) * up
            }
            _ => from[2] + (to[2] - from[2]) * up,
        }
    }

    /// Climbs onto a top: the clip fraction at which the path's height first
    /// clears the lip, and how far along (fraction) the body may be before it.
    fn lip_clear(&self) -> (f32, f32) {
        let to = self.mec.move_to;
        let base = self.lip_limit(to[2] - 1.0);
        if base >= 1.0 {
            return (0.0, 1.0);
        }
        const N: usize = 64;
        for i in 0..=N {
            let f = i as f32 / N as f32;
            if self.script_z(f) >= to[2] - 0.25 {
                return (f, base);
            }
        }
        (1.0, base)
    }

    /// Velocity the scripted move hands back at its end: the stored exit
    /// speed, plus (vault over) the fall of the clip's last keys.
    fn script_exit_velocity(&self) -> V3 {
        let exit = self.mec.exit_velocity;
        let over = matches!(self.mec.wall_side, sk::VAULT_OVER | sk::VAULT_OVER_LONG);
        let vz = if over {
            let total = self.mec.move_ms.max(1) as f32 / 1000.0;
            let a = self.script_point(0.95);
            let b = self.script_point(1.0);
            ((b[2] - a[2]) / (0.05 * total)).min(0.0)
        } else {
            0.0
        };
        [exit[0], exit[1], vz]
    }

    /// Momentum blend for the scripted path at `s` seconds into a move of
    /// `total` seconds: an offset that is zero at both ends and makes the
    /// path's velocity start at the entry velocity (`wall_normal`, stored at
    /// the start) and finish at the exit velocity, easing into / out of the
    /// clip's own speed over `BLEND_H` (horizontal) / `BLEND_Z` (vertical).
    /// The clip's shape and end point are unchanged; only the stall-then-lurch
    /// at the seams goes.
    fn script_blend(&self, s: f32, total: f32) -> V3 {
        let h = (1.0 / 60.0_f32).min(total * 0.25).max(1.0e-3);
        let fh = h / total;
        let v_start = scale(sub(self.script_point(fh), self.script_point(0.0)), 1.0 / h);
        let v_end = scale(sub(self.script_point(1.0), self.script_point(1.0 - fh)), 1.0 / h);
        let clamp_len = |v: V3| {
            let l = len(v);
            if l > BLEND_MAX { scale(v, BLEND_MAX / l) } else { v }
        };
        let a = clamp_len(sub(self.mec.wall_normal, v_start));
        let b = clamp_len(sub(self.script_exit_velocity(), v_end));
        // g(0) = 0, g'(0) = 1, g(w) = g'(w) = 0.
        let g = |x: f32, w: f32| {
            if x <= 0.0 || x >= w {
                0.0
            } else {
                let k = 1.0 - x / w;
                x * k * k
            }
        };
        let wh = BLEND_H.min(total / 3.0);
        let wz = BLEND_Z.min(total / 4.0);
        let r = (total - s).max(0.0);
        [
            a[0] * g(s, wh) - b[0] * g(r, wh),
            a[1] * g(s, wh) - b[1] * g(r, wh),
            a[2] * g(s, wz) - b[2] * g(r, wz),
        ]
    }

    /// Climb onto a top (vault onto, ledge): the target is the face plus a
    /// half width and an inch (probe geometry), so the hull stays behind the
    /// face (this fraction of the path) while its bottom (`z`) is below the
    /// lip; the path never drives into the obstacle. 1.0 once above it.
    fn lip_limit(&self, z: f32) -> f32 {
        let from = self.mec.move_from;
        let to = self.mec.move_to;
        let hl = hlen(horiz(sub(to, from)));
        if z >= to[2] - 0.25 || hl <= 1.0 {
            return 1.0;
        }
        ((hl - 2.0 * self.hw() - 1.5) / hl).max(0.0)
    }

    /// Vault → Springboard (VaultToSpringboard.IsAllowed): a jump pressed
    /// during the vault, or buffered before it, branches into the springboard
    /// in the vault's SpringboardBranch window when the obstacle top is
    /// 0.9–1.5 m above the vault start.
    fn try_vault_springboard(&mut self) -> bool {
        let t = *self.t;
        let (w0, w1) = match self.mec.wall_side {
            sk::VAULT_OVER => t.springboard_branch_over,
            sk::VAULT_OVER_LONG => t.springboard_branch_over_long,
            sk::VAULT_ONTO => t.springboard_branch_onto,
            _ => return false,
        };
        let s = self.mec.mode_ms as f32 / 1000.0;
        let buffered = self.mec.jump_press_ms as f32
            <= self.mec.mode_ms as f32 + t.springboard_sighting_time * 1000.0;
        if !buffered || s + 1.0e-4 < w0 || s > w1 + 1.0e-4 {
            return false;
        }
        let top = self.mec.move_mid[2] - t.vault_clearance;
        let h = top - self.mec.move_from[2];
        let speed = t.run_speed_at(self.mec.momentum * t.run_curve_time);
        if h < t.springboard_obstacle_min
            || h > t.springboard_obstacle_max
            || speed < t.springboard_min_speed
        {
            return false;
        }
        let dir = norm(horiz(sub(self.mec.move_to, self.mec.move_from)));
        let ground_z = self.mec.move_from[2];
        self.mec.wall_side = 0;
        self.start_springboard(dir, speed, top, None, ground_z);
        true
    }

    fn tick_scripted(&mut self, dt: f32) {
        if self.mec.mode == MecMode::Vault && self.try_vault_springboard() {
            return;
        }
        let total = self.mec.move_ms.max(1);
        let f = clampf(self.mec.mode_ms as f32 / total as f32, 0.0, 1.0);
        let total_s = total as f32 / 1000.0;
        let mut want = add(
            self.script_point(f),
            self.script_blend(f * total_s, total_s),
        );
        if matches!(self.mec.wall_side, sk::VAULT_OVER | sk::VAULT_OVER_LONG) {
            // The clip starts down while the body can still be over the
            // obstacle (the top holds it up, the path keeps sinking below
            // it): never drop faster than the clip itself descends from
            // where the body actually is, so clearing the far edge is a
            // smooth step down, not a fall of the whole gap in one tick.
            let f0 = clampf((self.mec.mode_ms as f32 - dt * 1000.0) / total as f32, 0.0, 1.0);
            let prev = add(
                self.script_point(f0),
                self.script_blend(f0 * total_s, total_s),
            );
            let dz = want[2] - prev[2];
            if dz < 0.0 {
                want[2] = want[2].max(self.ps.origin[2] + dz);
            }
        } else {
            // This step starts below the lip: stay behind the face for it too.
            let from = self.mec.move_from;
            let h = horiz(sub(self.mec.move_to, from));
            // Never pulled back: a body already past the limit holds there.
            let now = dot(horiz(sub(self.ps.origin, from)), h) / dot(h, h).max(1.0e-6);
            let limit = self.lip_limit(self.ps.origin[2].min(want[2])).max(now);
            let done = dot(horiz(sub(want, from)), h) / dot(h, h).max(1.0e-6);
            if done > limit {
                want = [from[0] + h[0] * limit, from[1] + h[1] * limit, want[2]];
            }
        }
        let vel = scale(sub(want, self.ps.origin), 1.0 / dt.max(1.0e-4));
        let out = slide_move(
            self.c,
            self.ps.origin,
            vel,
            dt,
            self.stand_hull(),
            None,
            self.walk_normal(),
        );
        // Report what the body actually did: when the clip's fall runs into
        // the floor behind a vault, the asked-for velocity points into it.
        self.ps.velocity = scale(sub(out.origin, self.ps.origin), 1.0 / dt.max(1.0e-4));
        self.ps.origin = out.origin;
        if f >= 1.0 {
            let kind = self.mec.wall_side;
            let over = matches!(kind, sk::VAULT_OVER | sk::VAULT_OVER_LONG);
            if !over {
                // Settle on the ledge top if the path was nudged by collision.
                let to = self.mec.move_to;
                if len(sub(to, self.ps.origin)) > 0.5 {
                    let tr = trace(self.c, self.ps.origin, to, self.stand_hull());
                    if tr.startsolid == 0 && tr.fraction >= 1.0 {
                        self.ps.origin = to;
                    }
                }
            }
            // The vault's last keys are falling: carry that into the air.
            let exit = self.script_exit_velocity();
            self.ps.velocity = exit;
            self.mec.air_flags = 0;
            self.mec.wall_side = 0;
            self.mec.wall_normal = ZERO;
            // A vault over that ends a step above the floor falls the rest
            // (no snap down onto it); climbs settle onto the top.
            if self.find_ground(!over).is_some() {
                self.ps.velocity[2] = 0.0;
                self.mec.momentum = self.t.run_time_for(hlen(exit)) / self.t.run_curve_time;
                // On the top: a landing, so the next wall (even one facing the
                // same way as the one just climbed) is a new wall.
                self.mec.last_wall_normal = ZERO;
                self.set_mode(MecMode::Ground);
            } else {
                self.enter_air();
                self.mec.momentum = self.t.run_time_for(hlen(exit)) / self.t.run_curve_time;
            }
        }
    }

    // ---------------------------------------------------------- slide/roll
    fn tick_slide(&mut self, input: &Input, dt: f32) {
        let t = *self.t;
        self.mec.crouched = true;
        let Some(normal) = self.find_ground(self.ps.velocity[2] <= 1.0) else {
            self.res.push(MecEvent::SlideEnd);
            self.enter_air();
            self.tick_air(input, dt);
            return;
        };
        if input.pressed(buttons::JUMP) {
            self.res.push(MecEvent::SlideEnd);
            self.enter_air();
            self.consume_jump();
            self.ps.velocity[2] = t.jump_velocity(t.jump_height);
            self.res.push(MecEvent::Jump);
            self.tick_air(input, dt);
            return;
        }
        // WalkSetting.Slide speed curve, entered where the current speed sits on it.
        let vel = horiz(self.ps.velocity);
        let speed = hlen(vel);
        let ns = t.slide_speed_at(t.slide_time_for(speed) + dt).min(speed);
        let vel = if speed > 0.0 {
            scale(vel, ns / speed)
        } else {
            vel
        };
        let out = step_slide_move(
            self.c,
            self.ps.origin,
            vel,
            dt,
            self.hull(),
            Some(normal),
            t.step_height,
            self.walk_normal(),
        );
        self.ps.origin = out.origin;
        self.ps.velocity = [out.velocity[0], out.velocity[1], 0.0];
        self.cap_momentum();

        let released = !input.held(buttons::CROUCH) && self.mec.mode_ms >= SLIDE_MIN_MS;
        let s = hlen(self.ps.velocity);
        let expired = self.mec.mode_ms as f32 >= t.slide_max_time * 1000.0;
        // AbortSlide: speed < 4 m/s.
        if s < t.slide_min_speed || expired || released {
            self.res.push(MecEvent::SlideEnd);
            self.set_mode(MecMode::Ground);
            self.mec.crouched =
                input.held(buttons::CROUCH) || !fits(self.c, self.ps.origin, self.stand_hull());
        }
        if self.find_ground(true).is_none() {
            self.res.push(MecEvent::SlideEnd);
            self.enter_air();
        }
    }

    fn start_roll(&mut self, input: &Input, drop: f32) {
        let vh = horiz(self.ps.velocity);
        let dir = if hlen(vh) > IS_MOVING * 10.0 {
            norm(vh)
        } else {
            yaw_axes(input.yaw).0
        };
        self.mec.crouched = true;
        self.set_mode(MecMode::Roll);
        self.mec.exit_velocity = dir;
        // Entry speed (in/s) the roll carries, kept in `wall_normal[0]`.
        let entry = hlen(vh).min(self.t.run_max_speed.max(self.roll_exit_speed()));
        self.mec.wall_normal = [entry, 0.0, 0.0];
        self.mec.move_ms = (self.t.roll_move_out * 1000.0) as i32;
        self.ps.velocity = scale(dir, entry);
        self.res.push(MecEvent::Roll { fall_height: drop });
    }

    /// FallingLandRoll speed at the move-out (in/s): what the roll hands back.
    fn roll_exit_speed(&self) -> f32 {
        let scale_m = self.t.roll_dist / LAND_ROLL.total_forward();
        LAND_ROLL.forward_speed(self.t.roll_move_out) * scale_m
    }

    /// Roll speed `s` seconds in: the landing speed eased into the clip's
    /// move-out speed, with a shallow mid-roll slow-down (`ROLL_SAG`). Equal
    /// to the entry speed at 0 and the exit speed at the move-out, with zero
    /// slope at both ends, so the body neither stalls nor lurches.
    fn roll_speed(&self, s: f32) -> f32 {
        let u = clampf(s / self.t.roll_move_out.max(1.0e-3), 0.0, 1.0);
        let v0 = self.mec.wall_normal[0];
        let v1 = self.roll_exit_speed();
        let ease = u * u * (3.0 - 2.0 * u);
        let sag = libm::sinf(core::f32::consts::PI * u);
        (v0 + (v1 - v0) * ease) * (1.0 - ROLL_SAG * sag * sag)
    }

    /// FallingLandRoll: carries the landing momentum through the roll (the
    /// clip's root motion keeps its exit speed, ~5.5 m/s) and returns control
    /// at the 1.0 s move-out at that speed.
    fn tick_roll(&mut self, input: &Input, dt: f32) {
        let t = *self.t;
        self.mec.crouched = true;
        let Some(normal) = self.find_ground(true) else {
            self.mec.wall_normal = ZERO;
            self.enter_air();
            return;
        };
        let dir = self.mec.exit_velocity;
        let s1 = self.mec.mode_ms as f32 / 1000.0;
        let s0 = (s1 - dt).max(0.0);
        let vel = scale(dir, self.roll_speed(0.5 * (s0 + s1)));
        let out = step_slide_move(
            self.c,
            self.ps.origin,
            vel,
            dt,
            self.hull(),
            Some(normal),
            t.step_height,
            self.walk_normal(),
        );
        self.ps.origin = out.origin;
        self.ps.velocity = [out.velocity[0], out.velocity[1], 0.0];
        if self.mec.mode_ms >= self.mec.move_ms {
            let exit = self.roll_speed(s1);
            let blocked = hlen(self.ps.velocity) < hlen(vel) * 0.5;
            let v = if blocked { ZERO } else { scale(dir, exit) };
            self.ps.velocity = v;
            self.mec.momentum = t.run_time_for(hlen(v)) / t.run_curve_time;
            self.cap_momentum();
            self.mec.exit_velocity = ZERO;
            self.mec.wall_normal = ZERO;
            self.set_mode(MecMode::Ground);
            self.mec.crouched =
                input.held(buttons::CROUCH) || !fits(self.c, self.ps.origin, self.stand_hull());
        }
    }

    fn tick_hard_landing(&mut self, input: &Input, dt: f32) {
        let t = *self.t;
        self.mec.momentum = 0.0;
        let Some(normal) = self.find_ground(true) else {
            self.enter_air();
            return;
        };
        if self.mec.wall_side == sk::STUMBLE {
            // FallingLandFailNoDamage: a stumble, walk speed at most.
            let (wish, mag) = input.wish();
            let mut vel = horiz(self.ps.velocity);
            let diff = sub(scale(wish, t.stumble_speed * mag), vel);
            let dl = len(diff);
            let max = t.ground_decel * dt;
            vel = if dl > max {
                mad(vel, diff, max / dl)
            } else {
                add(vel, diff)
            };
            let out = step_slide_move(
                self.c,
                self.ps.origin,
                vel,
                dt,
                self.hull(),
                Some(normal),
                t.step_height,
                self.walk_normal(),
            );
            self.ps.origin = out.origin;
            self.ps.velocity = [out.velocity[0], out.velocity[1], 0.0];
        } else {
            self.ps.velocity = ZERO;
        }
        if self.mec.mode_ms >= self.mec.move_ms {
            self.set_mode(MecMode::Ground);
        }
    }

    // --------------------------------------------------------------- output
    fn finish(&mut self) {
        let b = self.ctx.bounds;
        let t = self.t;
        let hull = self.hull();
        self.res.mins = hull.mins;
        self.res.maxs = hull.maxs;
        self.res.view_height = if self.mec.crouched {
            b.crouch_view_height
        } else {
            b.stand_view_height
        };
        if self.mec.crouched {
            self.ps.pm_flags |= pm_flags::CROUCH;
        } else {
            self.ps.pm_flags &= !pm_flags::CROUCH;
        }
        if !matches!(
            self.mec.mode,
            MecMode::Ground | MecMode::Slide | MecMode::Roll | MecMode::HardLanding
        ) {
            self.ps.ground_entity_num = ENTITYNUM_NONE;
        }
        self.res.walking = self.ps.ground_entity_num != ENTITYNUM_NONE;
        self.res.camera_roll = self.mec.camera_roll(t);
        let (fire, ads, lowered) = match self.mec.mode {
            MecMode::Ground | MecMode::Air | MecMode::Slide => (true, true, false),
            MecMode::WallRun => (true, false, false),
            MecMode::WallClimb
            | MecMode::LedgeClimb
            | MecMode::Vault
            | MecMode::Roll
            | MecMode::HardLanding => (false, false, true),
        };
        // Bring the weapon back up over the end of a timed move (vault, ledge,
        // roll, hard landing), not in the tick after it.
        let timed_end = matches!(
            self.mec.mode,
            MecMode::LedgeClimb | MecMode::Vault | MecMode::Roll | MecMode::HardLanding
        ) && self.mec.move_ms > 0
            && self.mec.mode_ms >= self.mec.move_ms - RAISE_LEAD_MS;
        let lowered = lowered && !timed_end;
        self.res.gates = WeaponGates {
            allow_fire: fire,
            allow_ads: ads && self.mec.turn_remaining == 0.0,
            lowered,
        };
        let s = hlen(self.ps.velocity);
        let x = clampf(
            (s - t.shield_min_speed) / (t.shield_full_speed - t.shield_min_speed),
            0.0,
            1.0,
        );
        // FaithShield uses a sine ease-in-out over this range.
        self.res.shield = 0.5 - 0.5 * libm::cosf(x * core::f32::consts::PI);
    }
}

fn lerp1((a, b): (f32, f32), f: f32) -> f32 {
    a + (b - a) * clampf(f, 0.0, 1.0)
}

fn wrap180(a: f32) -> f32 {
    let mut a = a;
    while a > 180.0 {
        a -= 360.0;
    }
    while a <= -180.0 {
        a += 360.0;
    }
    a
}
