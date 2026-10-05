//! Third-person Catalyst moves: which of Faith's clips (`mec_anim` pack) a body plays for
//! its `MecMoveState`, at what time, with what blend, and the retarget onto its DObj.
//!
//! * Clip time follows the move: scripted / timed moves (vault, ledge climb, wallclimb,
//!   roll, hard landing) map `mode_ms / duration` onto the whole clip, so the clip ends when
//!   the decoded move ends; wallrun, slide and jump play 1:1 from the move start; fall and
//!   sprint loop (sprint stride matched to the body's ground speed).
//! * Snapshot `mode_ms` only advances per snapshot, so a local clock advances by the frame
//!   time and is pulled back to the move's own time when it drifts more than a tick.
//! * Crossfade between clips and in / out of Catalyst control: [`BLEND_SECONDS`].
//! * Moves that lower the weapon (vault, climbs, roll, hard landing) drive the whole body;
//!   the others (wallrun, slide, jump / fall, landing, sprint) drive hips, legs and spine
//!   and leave the arms on the MW2 weapon-hold animation.
//! * No pack: nothing here runs; the stock-clip mapping stays as it was.

use std::collections::HashMap;
use std::sync::Arc;

use mec_anim::{Pack, Part, Retarget, SrcPose};
use sim::{MecMode, MecMoveState};

/// Crossfade / fade-in / fade-out time (s).
pub const BLEND_SECONDS: f32 = 0.15;
/// Inches per metre.
const INCH: f32 = 39.3701;
/// Ground speed (in/s, forward) from which the Catalyst sprint loop replaces the MW2 run.
const SPRINT_FROM: f32 = 150.0;
/// Time (s) after touching down that a landing clip plays when the drop was real.
const LANDING_SECONDS: f32 = 0.6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BodyMask {
    /// Every mapped bone (weapon lowered).
    Full,
    /// Hips, legs, spine and head; arms stay on the MW2 animation.
    Lower,
}

impl BodyMask {
    fn weight(self, part: Part) -> f32 {
        match (self, part) {
            (Self::Full, _) => 1.0,
            (Self::Lower, Part::Arm) => 0.0,
            (Self::Lower, Part::Spine | Part::Head) => 0.7,
            (Self::Lower, _) => 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drive {
    /// Clip position follows the move: u = (elapsed / length), clamped.
    Move { elapsed: f32, length: f32 },
    /// Free-running loop at `rate` clip lengths per clip length.
    Loop { rate: f32 },
}

#[derive(Clone, Copy, Debug)]
struct Choice {
    clip: usize,
    drive: Drive,
    mask: BodyMask,
    /// Changes when the same clip must restart (new move of the same kind).
    key: (u8, i8),
}

#[derive(Clone, Copy, Debug)]
struct Playing {
    clip: usize,
    /// Position 0..=1 in the clip.
    u: f32,
    /// Local clock (s) since the clip started.
    clock: f32,
    mask: BodyMask,
    key: (u8, i8),
}

#[derive(Default)]
struct EntAnim {
    cur: Option<Playing>,
    prev: Option<Playing>,
    /// 0 → 1 crossfade from `prev` to `cur`.
    fade: f32,
    /// Overall Catalyst weight (fades in / out).
    weight: f32,
    last_origin: Option<[f32; 3]>,
    /// Smoothed forward ground speed (in/s).
    speed: f32,
    mode: u8,
    /// Mode before the current one, and the wall side when it was a wallrun.
    from_mode: u8,
    from_side: i8,
    /// Highest point of the current airtime above the landing (in), for landing clips.
    drop: f32,
    land_drop: f32,
    last_mode_ms: i32,
    coil_clock: f32,
}

/// What `pose` needs this frame.
pub struct MecBodyPlan {
    pack: &'static Pack,
    a: Option<(usize, f32, BodyMask)>,
    b: (usize, f32, BodyMask),
    fade: f32,
    pub weight: f32,
}

impl MecBodyPlan {
    /// Weight the MW2 aim-pitch controller keeps (full-body moves own the spine).
    pub fn aim_keep(&self) -> f32 {
        let full = |m: BodyMask| if m == BodyMask::Full { 1.0 } else { 0.0 };
        let f = match self.a {
            Some((_, _, ma)) => full(ma) * (1.0 - self.fade) + full(self.b.2) * self.fade,
            None => full(self.b.2),
        };
        1.0 - f * self.weight
    }
}

#[derive(Default, bevy::prelude::Resource)]
pub struct MecBodyAnims {
    ents: HashMap<u32, EntAnim>,
    retargets: HashMap<u64, Option<Arc<Retarget>>>,
    logged: bool,
}

fn clip_by(pack: &Pack, names: &[&str]) -> Option<usize> {
    names.iter().find_map(|n| pack.clip(n))
}

impl MecBodyAnims {
    pub fn retain_live(&mut self, live: &std::collections::HashSet<u32>) {
        self.ents.retain(|k, _| live.contains(k));
    }

    /// Advance one body; `None` when no Catalyst clip should touch it (no pack, not a
    /// Catalyst mover, or faded out).
    pub fn update(
        &mut self,
        key: u32,
        mec: Option<&MecMoveState>,
        origin: [f32; 3],
        yaw_deg: f32,
        dt: f32,
    ) -> Option<MecBodyPlan> {
        if !self.logged {
            self.logged = true;
            bevy::log::info!("mec_anim: {}", mec_anim::global_status());
        }
        let pack = Pack::global()?;
        let Some(m) = mec else {
            self.ents.remove(&key);
            return None;
        };
        let st = self.ents.entry(key).or_default();
        let dt = dt.clamp(0.0, 0.1);

        // ground speed along the facing
        if let Some(prev) = st.last_origin
            && dt > 1e-4
        {
            let (s, c) = yaw_deg.to_radians().sin_cos();
            let v = ((origin[0] - prev[0]) * c + (origin[1] - prev[1]) * s) / dt;
            let jump = ((origin[0] - prev[0]).powi(2) + (origin[1] - prev[1]).powi(2)).sqrt();
            if jump < 64.0 {
                let k = 1.0 - (-8.0 * dt).exp();
                st.speed += (v - st.speed) * k;
            }
        }
        st.last_origin = Some(origin);

        let mode = m.mode as u8;
        if mode != st.mode {
            st.from_mode = st.mode;
            st.from_side = if st.mode == MecMode::WallRun as u8 {
                st.from_side
            } else {
                0
            };
            if st.mode == MecMode::Air as u8 {
                st.land_drop = (st.drop - origin[2]).max(0.0);
            }
            st.mode = mode;
        }
        if m.mode == MecMode::Air {
            st.drop = st.drop.max(origin[2]);
        } else {
            st.drop = origin[2];
        }
        if m.mode == MecMode::WallRun {
            st.from_side = m.wall_side;
        }
        if m.coil {
            st.coil_clock += dt;
        } else {
            st.coil_clock = 0.0;
        }
        let restarted = m.mode_ms < st.last_mode_ms - 60;
        st.last_mode_ms = m.mode_ms;

        let choice = choose(pack, m, st);
        match choice {
            Some(ch) => {
                let same = st
                    .cur
                    .as_ref()
                    .is_some_and(|c| c.clip == ch.clip && c.key == ch.key && !restarted);
                if !same {
                    if st.weight > 0.01 {
                        st.prev = st.cur;
                        st.fade = 0.0;
                    } else {
                        st.prev = None;
                        st.fade = 1.0;
                    }
                    st.cur = Some(Playing {
                        clip: ch.clip,
                        u: 0.0,
                        clock: match ch.drive {
                            Drive::Move { elapsed, .. } => elapsed,
                            Drive::Loop { .. } => 0.0,
                        },
                        mask: ch.mask,
                        key: ch.key,
                    });
                }
                let cur = st.cur.as_mut().expect("set above");
                let len = pack.clips[cur.clip].seconds;
                cur.mask = ch.mask;
                match ch.drive {
                    Drive::Move { elapsed, length } => {
                        cur.clock += dt;
                        if (cur.clock - elapsed).abs() > 0.1 {
                            cur.clock = elapsed;
                        }
                        cur.u = (cur.clock / length.max(0.05)).clamp(0.0, 1.0);
                    }
                    Drive::Loop { rate } => {
                        cur.clock += dt;
                        cur.u = (cur.u + dt * rate / len).rem_euclid(1.0);
                    }
                }
                st.weight = (st.weight + dt / BLEND_SECONDS).min(1.0);
            }
            None => {
                st.weight = (st.weight - dt / BLEND_SECONDS).max(0.0);
                if st.weight <= 0.0 {
                    st.cur = None;
                    st.prev = None;
                }
            }
        }
        if st.prev.is_some() {
            st.fade = (st.fade + dt / BLEND_SECONDS).min(1.0);
            if st.fade >= 1.0 {
                st.prev = None;
            }
        }
        let cur = st.cur?;
        if st.weight <= 0.0 {
            return None;
        }
        Some(MecBodyPlan {
            pack,
            a: st.prev.map(|p| (p.clip, p.u, p.mask)),
            b: (cur.clip, cur.u, cur.mask),
            fade: if st.prev.is_some() { st.fade } else { 1.0 },
            weight: st.weight,
        })
    }

    /// Write the planned Catalyst pose into `locals` (call before the player controller).
    pub fn pose(
        &mut self,
        plan: &MecBodyPlan,
        dobj: &xmodel_runtime::DObj,
        locals: &mut [anim_iw4::Local],
    ) {
        let pack = plan.pack;
        let sig = skeleton_signature(dobj);
        let Some(rt) = self
            .retargets
            .entry(sig)
            .or_insert_with(|| {
                let rt = Retarget::new(&pack.skeleton, dobj);
                match &rt {
                    Some(rt) => bevy::log::info!(
                        "mec_anim: retarget onto a {}-bone body: {} bones mapped, hips scale {:.3}",
                        dobj.bones.len(),
                        rt.mapped,
                        rt.scale
                    ),
                    None => bevy::log::warn!(
                        "mec_anim: body with {} bones has no j_mainroot / j_hip_le / j_spine4; stock clips only",
                        dobj.bones.len()
                    ),
                }
                rt.map(Arc::new)
            })
            .clone()
        else {
            return;
        };
        let mut pb = SrcPose::default();
        pack.clips[plan.b.0].sample(plan.b.1, &mut pb);
        let pose = match plan.a {
            Some((clip, u, _)) if plan.fade < 1.0 => {
                let mut pa = SrcPose::default();
                pack.clips[clip].sample(u, &mut pa);
                let mut out = SrcPose::default();
                mec_anim::blend_pose(&pa, &pb, plan.fade, &mut out);
                out
            }
            _ => pb,
        };
        let (ma, mb, fade, w) = (plan.a.map(|a| a.2), plan.b.2, plan.fade, plan.weight);
        rt.apply(&pack.skeleton, &pose, dobj, locals, |part| {
            let wb = mb.weight(part);
            let wpart = match ma {
                Some(ma) if fade < 1.0 => ma.weight(part) * (1.0 - fade) + wb * fade,
                _ => wb,
            };
            wpart * w
        });
    }
}

fn skeleton_signature(dobj: &xmodel_runtime::DObj) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for b in &dobj.bones {
        b.name.hash(&mut h);
        b.parent.hash(&mut h);
        for x in b.bind_translation.to_array() {
            x.to_bits().hash(&mut h);
        }
    }
    h.finish()
}

/// The clip for this move state, or `None` (stock MW2 animation).
fn choose(pack: &Pack, m: &MecMoveState, st: &EntAnim) -> Option<Choice> {
    use movement_mec::script_kind as k;
    let tuning = movement_mec::MecTuning::DEFAULT;
    let elapsed = m.mode_ms.max(0) as f32 / 1000.0;
    let move_len = m.camera_clip(elapsed, &tuning).duration;
    let mode = m.mode as u8;
    let one_to_one = |clip: usize| Drive::Move {
        elapsed,
        length: pack.clips[clip].seconds,
    };
    let stretched = |clip: usize| Drive::Move {
        elapsed,
        length: if move_len > 0.05 {
            move_len
        } else {
            pack.clips[clip].seconds
        },
    };
    let pick = |names: &[&str], drive: &dyn Fn(usize) -> Drive, mask: BodyMask, kind: i8| {
        clip_by(pack, names).map(|clip| Choice {
            clip,
            drive: drive(clip),
            mask,
            key: (mode, kind),
        })
    };
    match m.mode {
        MecMode::WallRun => {
            let names: &[&str] = if m.wall_side < 0 {
                &["WallRunLeft"]
            } else {
                &["WallRunRight"]
            };
            pick(names, &one_to_one, BodyMask::Lower, m.wall_side)
        }
        MecMode::WallClimb => pick(&["WallRunVertical"], &stretched, BodyMask::Full, 0),
        MecMode::Vault => {
            let names: &[&str] = match m.wall_side {
                k::VAULT_OVER => &["VaultOverFast", "VaultOver"],
                k::VAULT_OVER_LONG => &["VaultOverFastLong", "VaultOver", "VaultOverFast"],
                _ => &["VaultOnto", "VaultOntoFast"],
            };
            pick(names, &stretched, BodyMask::Full, m.wall_side)
        }
        MecMode::LedgeClimb => {
            let names: &[&str] = match m.wall_side {
                k::LEDGE_LOW => &["VaultOnto", "VaultOntoFast"],
                _ => &["VaultOntoHigh", "HangHeaveUp"],
            };
            pick(names, &stretched, BodyMask::Full, m.wall_side)
        }
        MecMode::Roll => pick(&["FallingLandRoll"], &stretched, BodyMask::Full, 0),
        MecMode::HardLanding => {
            let names: &[&str] = match m.wall_side {
                k::STUMBLE => &["FallingLandFailNoDamage"],
                k::FAIL_MEDIUM => &["FallingLandFailMedium"],
                k::FAIL => &["FallingLandFail", "FallingLandFailMedium"],
                _ => return None,
            };
            pick(names, &stretched, BodyMask::Full, m.wall_side)
        }
        MecMode::Slide => pick(&["CrouchSlide"], &one_to_one, BodyMask::Lower, 0),
        MecMode::Air => {
            if m.coil
                && let Some(clip) = pack.clip("JumpCoil")
            {
                return Some(Choice {
                    clip,
                    drive: Drive::Move {
                        elapsed: st.coil_clock,
                        length: pack.clips[clip].seconds,
                    },
                    mask: BodyMask::Lower,
                    key: (mode, 9),
                });
            }
            let from = MecMode::from_u8(st.from_mode);
            let jump: &[&str] = match from {
                MecMode::WallRun if st.from_side < 0 => &["WallRunJumpLeft"],
                MecMode::WallRun => &["WallRunJumpRight"],
                MecMode::Ground | MecMode::Slide => &["JumpFast3"],
                _ => &[],
            };
            if let Some(clip) = clip_by(pack, jump)
                && elapsed < pack.clips[clip].seconds
            {
                return Some(Choice {
                    clip,
                    drive: one_to_one(clip),
                    mask: BodyMask::Lower,
                    key: (mode, 1),
                });
            }
            pick(
                &["Fall", "JumpFastLoop"],
                &|_| Drive::Loop { rate: 1.0 },
                BodyMask::Lower,
                2,
            )
        }
        MecMode::Ground => {
            let from = MecMode::from_u8(st.from_mode);
            if from == MecMode::Air && st.land_drop > 40.0 && elapsed < LANDING_SECONDS {
                let names: &[&str] = if st.speed > SPRINT_FROM {
                    &["RunFwdLand", "Loco_Land"]
                } else {
                    &["Loco_Land", "RunFwdLand"]
                };
                if let Some(c) = pick(names, &one_to_one, BodyMask::Lower, 3) {
                    return Some(c);
                }
            }
            if st.speed > SPRINT_FROM && m.momentum > 0.0 {
                let clip = pack.clip("Loco_Sprint_Fwd")?;
                let c = &pack.clips[clip];
                // stride: the clip covers `distance_m` per loop
                let clip_speed = (c.distance_m * INCH / c.seconds).max(1.0);
                let rate = if c.distance_m > 0.1 {
                    (st.speed / clip_speed).clamp(0.5, 1.6)
                } else {
                    1.0
                };
                return Some(Choice {
                    clip,
                    drive: Drive::Loop { rate },
                    mask: BodyMask::Lower,
                    key: (mode, 4),
                });
            }
            None
        }
    }
}
