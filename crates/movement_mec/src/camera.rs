//! First-person camera / viewmodel motion and third-person body lean per move,
//! timed by the decoded clip lengths (`MecTuning`). Pure functions of the move
//! (mode, script kind, seconds since it began, its duration), so the client
//! can evaluate them every frame from a smoothed clock.
//!
//! Conventions (IW4): pitch + = look down, roll + = roll right, `z` inches up.
//! Viewmodel offsets are in view space: `gun_drop` inches down, `gun_back`
//! inches toward the eye, angles in degrees.

use crate::state::{MecMode, script_kind as sk};
use crate::tuning::MecTuning;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MecCameraMotion {
    pub pitch: f32,
    pub roll: f32,
    pub z: f32,
    pub gun_drop: f32,
    pub gun_back: f32,
    pub gun_pitch: f32,
    pub gun_yaw: f32,
    pub gun_roll: f32,
    /// Third-person body roll (deg, + = top of the body to the right) and
    /// sideways shift of the feet (inches, + = right) — the wallrun lean.
    pub body_roll: f32,
    pub body_shift: f32,
}

/// The move a camera effect follows: what mode, which script kind / wall
/// side, how long it lasts (s; 0 = open-ended) and how long it has run (s).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveClip {
    pub mode: MecMode,
    pub kind: i8,
    pub duration: f32,
    pub elapsed: f32,
}

const PI: f32 = core::f32::consts::PI;

fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

/// Smoothstep 0→1.
fn smooth(x: f32) -> f32 {
    let x = clamp01(x);
    x * x * (3.0 - 2.0 * x)
}

/// 0 → 1 → 0 hump over `u` in 0..1.
fn hump(u: f32) -> f32 {
    let u = clamp01(u);
    libm::sinf(PI * u)
}

/// Fade in over `fin` s and out over the last `fout` s of `dur`.
fn envelope(e: f32, dur: f32, fin: f32, fout: f32) -> f32 {
    let a = if fin > 0.0 { clamp01(e / fin) } else { 1.0 };
    let b = if fout > 0.0 && dur > 0.0 {
        clamp01((dur - e) / fout)
    } else {
        1.0
    };
    smooth(a) * smooth(b)
}

/// Decaying shake (deg-ish amplitude 1) at `hz`.
fn shake(e: f32, hz: f32, decay: f32) -> f32 {
    libm::sinf(2.0 * PI * hz * e) * libm::expf(-decay * e)
}

/// Camera / viewmodel / body offsets for a move clip.
#[must_use]
pub fn camera_motion(clip: MoveClip, t: &MecTuning) -> MecCameraMotion {
    let e = clip.elapsed.max(0.0);
    let d = clip.duration;
    let u = if d > 0.0 { clamp01(e / d) } else { 0.0 };
    let mut m = MecCameraMotion::default();
    match clip.mode {
        MecMode::WallRun => {
            // Roll away from the wall over the 16 t align time, back out at the end.
            let side = f32::from(clip.kind.signum());
            let env = envelope(e, d, 16.0 * crate::TICK, 0.15);
            m.roll = -side * t.wallrun_camera_roll_deg * env;
            // Viewmodel: tilt with the camera plus a running sway (~1.5 steps/s per foot).
            let step = 2.0 * PI * 1.6 * e;
            m.gun_roll = (-side * 6.0 + 2.0 * libm::sinf(step)) * env;
            m.gun_yaw = 1.5 * libm::sinf(step * 0.5) * env;
            m.gun_drop = (0.6 + 0.4 * libm::fabsf(libm::sinf(step))) * env;
            // Body: feet on the wall, torso tilted off it.
            m.body_roll = -side * 20.0 * env;
            m.body_shift = side * 10.0 * env;
        }
        MecMode::WallClimb => {
            // Look up the wall while running up it; weapon lowered and tucked.
            let env = envelope(e, d, 0.2, 0.25);
            m.pitch = -8.0 * env;
            m.gun_drop = 6.0 * env;
            m.gun_pitch = 25.0 * env;
            m.gun_back = 3.0 * env;
        }
        MecMode::LedgeClimb => {
            // Heave: the eye follows the root path (body), the camera dips to
            // look over the lip around the curve's peak rise and comes back.
            let peak = crate::script_curve(clip.kind).peak_fraction();
            let look = if u < peak {
                smooth(u / peak.max(1.0e-3))
            } else {
                1.0 - smooth((u - peak) / (1.0 - peak).max(1.0e-3))
            };
            m.pitch = 14.0 * look;
            m.roll = 3.0 * hump(u);
            m.z = -4.0 * hump(u);
            let gun = envelope(e, d, 0.15, 0.3);
            m.gun_drop = 10.0 * gun;
            m.gun_pitch = 35.0 * gun;
            m.gun_roll = 15.0 * gun;
            m.gun_back = 4.0 * gun;
        }
        MecMode::Vault => {
            // Plant a hand: look down onto the obstacle at the rise, roll into
            // the swing, level out on the way down.
            let peak = crate::script_curve(clip.kind).peak_fraction();
            let look = if u < peak {
                smooth(u / peak.max(1.0e-3))
            } else {
                1.0 - smooth((u - peak) / (1.0 - peak).max(1.0e-3))
            };
            let onto = clip.kind == sk::VAULT_ONTO;
            m.pitch = if onto { 10.0 } else { 7.0 } * look;
            m.roll = if onto { 2.0 } else { -5.0 } * hump(u);
            let gun = envelope(e, d, 0.15, 0.25);
            m.gun_drop = 5.0 * gun;
            m.gun_roll = -25.0 * gun;
            m.gun_yaw = 6.0 * gun;
            m.gun_pitch = 12.0 * gun;
        }
        MecMode::Roll => {
            // FallingLandRoll: a full forward roll of the view over the clip
            // (1.167 s), fastest mid-roll; the eye drops toward the floor.
            let roll_t = t.roll_time;
            let ru = clamp01(e / roll_t);
            let spin = smooth(clamp01((ru - 0.06) / 0.72));
            m.pitch = 360.0 * spin;
            m.z = -18.0 * hump(clamp01(ru / 0.85));
            let gun = envelope(e, roll_t, 0.15, 0.35);
            m.gun_drop = 14.0 * gun;
            m.gun_pitch = 40.0 * gun;
        }
        MecMode::HardLanding => {
            let (depth, tilt, wobble, recover) = match clip.kind {
                sk::STUMBLE => (8.0, 7.0, 3.0, t.stumble_time),
                sk::FAIL_MEDIUM => (22.0, 16.0, 5.0, t.fail_medium_time),
                _ => (28.0, 24.0, 7.0, t.fail_time),
            };
            // Impact: drop fast, hold, come back up by the end of the clip.
            let down = smooth(e / 0.15);
            let up = smooth((e - recover * 0.55) / (recover * 0.45));
            let env = down * (1.0 - up);
            m.z = -depth * env;
            m.pitch = tilt * env + wobble * 0.5 * shake(e, 7.0, 4.0);
            m.roll = wobble * shake(e, 5.0, 3.0);
            m.gun_drop = 8.0 * env;
            m.gun_pitch = 20.0 * env;
            m.gun_roll = 10.0 * env;
        }
        MecMode::Slide => {
            // Lower than a crouch, slight lean back and a canted weapon.
            let env = envelope(e, 0.0, 0.18, 0.0);
            m.z = -8.0 * env;
            m.pitch = -2.0 * env;
            m.roll = 2.5 * env;
            m.gun_roll = -10.0 * env;
            m.gun_drop = 1.5 * env;
        }
        MecMode::Ground | MecMode::Air => {}
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(mode: MecMode, kind: i8, duration: f32, elapsed: f32) -> MoveClip {
        MoveClip {
            mode,
            kind,
            duration,
            elapsed,
        }
    }

    #[test]
    fn roll_view_turns_full_circle_over_the_clip() {
        let t = MecTuning::DEFAULT;
        let start = camera_motion(clip(MecMode::Roll, 0, t.roll_move_out, 0.0), &t);
        let mid = camera_motion(clip(MecMode::Roll, 0, t.roll_move_out, 0.45), &t);
        let end = camera_motion(clip(MecMode::Roll, 0, t.roll_move_out, t.roll_time), &t);
        assert!(start.pitch.abs() < 1.0);
        assert!(mid.pitch > 90.0 && mid.pitch < 300.0, "{}", mid.pitch);
        assert!((end.pitch - 360.0).abs() < 0.5, "{}", end.pitch);
        assert!(mid.z < -10.0 && end.z.abs() < 0.5);
    }

    #[test]
    fn wallrun_roll_is_six_degrees_away_from_the_wall() {
        let t = MecTuning::DEFAULT;
        let left = camera_motion(clip(MecMode::WallRun, -1, t.wallrun_time, 0.6), &t);
        let right = camera_motion(clip(MecMode::WallRun, 1, t.wallrun_time, 0.6), &t);
        assert!((left.roll - 6.0).abs() < 1e-3 && (right.roll + 6.0).abs() < 1e-3);
        assert!(left.body_roll > 10.0 && left.body_shift < 0.0);
        let end = camera_motion(
            clip(MecMode::WallRun, -1, t.wallrun_time, t.wallrun_time),
            &t,
        );
        assert!(end.roll.abs() < 1e-3);
    }

    #[test]
    fn hard_landings_recover_by_their_clip_end() {
        let t = MecTuning::DEFAULT;
        for (kind, dur) in [
            (sk::STUMBLE, t.stumble_time),
            (sk::FAIL_MEDIUM, t.fail_medium_time),
            (sk::FAIL, t.fail_time),
        ] {
            let hit = camera_motion(clip(MecMode::HardLanding, kind, dur, 0.2), &t);
            let end = camera_motion(clip(MecMode::HardLanding, kind, dur, dur), &t);
            assert!(hit.z < -5.0, "{kind}: {}", hit.z);
            assert!(end.z.abs() < 0.1 && end.gun_drop.abs() < 0.1);
        }
    }

    #[test]
    fn scripted_moves_settle_at_their_end() {
        let t = MecTuning::DEFAULT;
        for (mode, kind, dur) in [
            (MecMode::Vault, sk::VAULT_OVER, t.vault_short_time),
            (MecMode::LedgeClimb, sk::LEDGE_HIGH, t.ledge_climb_time),
        ] {
            let mid = camera_motion(clip(mode, kind, dur, dur * 0.4), &t);
            let end = camera_motion(clip(mode, kind, dur, dur), &t);
            assert!(mid.pitch > 3.0 && mid.gun_drop > 2.0);
            assert!(end.pitch.abs() < 0.1 && end.gun_drop.abs() < 0.1);
        }
    }
}
