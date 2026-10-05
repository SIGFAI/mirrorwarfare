//! The seam to the IW4 player step: runs [`mec_pmove`] in place of IW4
//! locomotion and keeps the IW4 bookkeeping the rest of the runtime reads
//! (ADS fraction, hold breath, view height, timers, footsteps, stance flags,
//! the sprint-lowered weapon) so the weapon, view and animation code stay
//! unchanged.

use movement_iw4::{
    CollisionBackend, Pml, PmoveSingleContext, drop_timers, footstep_event,
    footsteps_anim_move_type, footsteps_bob_cycle, should_make_footsteps, update_ads_frac,
    update_ads_intent, update_hold_breath, update_stance_target, update_view_height,
};
use playerstate_iw4::{PlayerState, UserCmd, buttons, eflags, pm_flags};

use crate::pmove::mec_pmove;
use crate::state::{MecContext, MecEvent, MecMode, MecMoveResult, MecMoveState, script_kind};
use crate::vec::hlen;

const ANIM_MT_IDLECR: u8 = 2;
const ANIM_MT_RUNCR: u8 = 12;
const ANIM_MT_CLIMBUP: u8 = 18;
const ANIM_MT_SPRINT: u8 = 20;
/// `mp_mantle_up_57` … `mp_mantle_up_21` (movetype = mantle xanim index + 21).
const ANIM_MT_MANTLE_UP_57: u8 = 22;
const MANTLE_UP_HEIGHTS: [f32; 7] = [57.0, 51.0, 45.0, 39.0, 33.0, 27.0, 21.0];
const ANIM_MT_MANTLE_OVER_HIGH: u8 = 29;
const ANIM_MT_MANTLE_OVER_MID: u8 = 30;
const ANIM_MT_MANTLE_OVER_LOW: u8 = 31;
const ANIM_MT_STUMBLE_SPRINT_FORWARD: u8 = 42;

/// Ground speed above which the run reads as a sprint on the third-person body.
const SPRINT_ANIM_SPEED: f32 = 215.0;
/// Landings from lower than this keep the running animation.
const LAND_ANIM_HEIGHT: f32 = 16.0;
/// Walking off an edge plays the fall animation once the drop is real.
const FALL_ANIM_MS: i32 = 200;
const FALL_ANIM_VZ: f32 = -150.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MecStep {
    pub result: MecMoveResult,
    /// Movement animation to request this tick (`None` keeps the current one).
    pub anim_movetype: Option<u8>,
    /// Restart the movement animation even when it is already playing.
    pub force_anim: bool,
    pub jumped: bool,
    /// Left a supported mode for free fall without a jump (walked off, let go).
    pub fell: bool,
    pub landed: bool,
    /// Collision traces this move ran (cost diagnostics).
    pub traces: u32,
}

/// One usercmd through Catalyst movement, with the IW4 player bookkeeping.
/// `cmd` is edited in place: buttons a parkour move forbids are removed so
/// the weapon step that follows sees what the player may do.
pub fn mec_pmove_iw4<C: CollisionBackend>(
    ps: &mut PlayerState,
    mec: &mut MecMoveState,
    cmd: &mut UserCmd,
    iw4: &PmoveSingleContext,
    collision: &C,
) -> MecStep {
    let msec = cmd.server_time.wrapping_sub(ps.command_time).clamp(1, 200);
    cmd.buttons &= !(buttons::SPRINT | buttons::PRONE);
    mec.old_buttons = iw4.old_buttons;

    let mut ctx = MecContext::new(msec);
    ctx.view_clamp = iw4.view_angles;
    ctx.speed_scale = speed_scale(ps, iw4);
    let mode_before = mec.mode;
    let air_ms_before = if mode_before == MecMode::Air {
        mec.mode_ms
    } else {
        0
    };
    let counted = CountingBackend {
        inner: collision,
        traces: core::cell::Cell::new(0),
    };
    let result = mec_pmove(ps, mec, cmd, &ctx, &counted);
    let traces = counted.traces.get();

    if !result.gates.allow_fire {
        cmd.buttons &= !(buttons::ATTACK | buttons::MELEE_CHARGE);
    }
    if !result.gates.allow_ads {
        cmd.buttons &= !buttons::ADS;
    }
    if result.gates.lowered {
        cmd.buttons &= !(buttons::RELOAD | buttons::USE_RELOAD | buttons::FRAG | buttons::SMOKE);
        ps.pm_flags |= pm_flags::SPRINTING;
    } else {
        ps.pm_flags &= !pm_flags::SPRINTING;
    }
    ps.pm_flags &= !(pm_flags::PRONE | pm_flags::MANTLE | pm_flags::LADDER);
    if mec.crouched {
        ps.e_flags = (ps.e_flags & !eflags::PRONE) | eflags::DUCK;
    } else {
        ps.e_flags &= !(eflags::DUCK | eflags::PRONE);
    }

    let pml = Pml {
        forward: [0.0; 3],
        right: [0.0; 3],
        up: [0.0; 3],
        frametime: msec as f32 * 0.001,
        msec,
        walking: u32::from(result.walking),
        ground_plane: u32::from(result.walking),
        almost_ground_plane: 0,
        ground_trace: [0; 11],
        previous_origin: ps.origin,
        previous_velocity: ps.velocity,
        holdrand: 0,
        jump_animations: [None; 4],
        mantle_movetype: None,
        landing_animation: false,
    };
    update_ads_intent(ps, cmd, iw4.old_buttons, iw4.ads_intent);
    update_ads_frac(ps, msec, iw4.ads_frac);
    update_hold_breath(ps, cmd.buttons, msec, iw4.can_hold_breath);
    let _ = update_stance_target(ps);
    update_view_height(ps, &pml, cmd);
    drop_timers(ps, &pml);

    if mec.mode == MecMode::Ground {
        let old_bob = ps.bob_cycle as u8;
        footsteps_bob_cycle(
            ps,
            msec,
            cmd.forwardmove,
            cmd.rightmove,
            false,
            cmd.server_time,
            iw4.walk.cmd_scale,
        );
        let make = should_make_footsteps(ps);
        footstep_event(ps, old_bob, ps.bob_cycle as u8, 0, make);
    }

    let (anim_movetype, force_anim) = anim_move_type(ps, mec, cmd);
    let jumped = result.iter_events().any(|e| {
        matches!(
            e,
            MecEvent::Jump | MecEvent::Springboard | MecEvent::WallJump
        )
    });
    let landed = result.iter_events().any(|e| {
        matches!(
            e,
            MecEvent::Land { fall_height } if fall_height >= LAND_ANIM_HEIGHT
        ) || matches!(e, MecEvent::Roll { .. } | MecEvent::HardLanding { .. })
    });
    let let_go = matches!(mode_before, MecMode::WallRun | MecMode::WallClimb);
    let dropped = air_ms_before < FALL_ANIM_MS
        && mec.mode_ms >= FALL_ANIM_MS
        && ps.velocity[2] < FALL_ANIM_VZ;
    MecStep {
        result,
        anim_movetype,
        force_anim,
        jumped,
        fell: !jumped && mec.mode == MecMode::Air && (let_go || dropped),
        landed,
        traces,
    }
}

/// Weapon move speed, blended toward the ADS move speed as the sight comes up.
fn speed_scale(ps: &PlayerState, iw4: &PmoveSingleContext) -> f32 {
    let scales = iw4.walk.cmd_scale;
    let base = if scales.weapon_move_speed_scale > 0.0 {
        scales.weapon_move_speed_scale
    } else {
        1.0
    };
    let ads = if scales.weapon_ads_move_speed_scale > 0.0 {
        scales.weapon_ads_move_speed_scale
    } else {
        1.0
    };
    let frac = ps.f_weapon_pos_frac.clamp(0.0, 1.0);
    base * (1.0 + (ads - 1.0) * frac)
}

/// Third-person movement animation for the current mode, from the stock
/// player animation set.
fn anim_move_type(ps: &PlayerState, mec: &MecMoveState, cmd: &UserCmd) -> (Option<u8>, bool) {
    let speed = hlen(ps.velocity);
    match mec.mode {
        MecMode::Ground => {
            if !mec.crouched && cmd.forwardmove > 0 && speed >= SPRINT_ANIM_SPEED {
                (Some(ANIM_MT_SPRINT), false)
            } else {
                (
                    footsteps_anim_move_type(ps, cmd.forwardmove, cmd.rightmove, false),
                    false,
                )
            }
        }
        MecMode::Slide => (Some(ANIM_MT_IDLECR), false),
        MecMode::Roll => (Some(ANIM_MT_RUNCR), false),
        MecMode::HardLanding if mec.wall_side == script_kind::STUMBLE => {
            (Some(ANIM_MT_STUMBLE_SPRINT_FORWARD), false)
        }
        MecMode::HardLanding => (Some(ANIM_MT_IDLECR), false),
        MecMode::WallRun => (Some(ANIM_MT_SPRINT), false),
        MecMode::WallClimb => (Some(ANIM_MT_CLIMBUP), false),
        // Scripted moves: the stock mantle clip closest to the obstacle; the
        // client scales its rate to the move length (`scripted_anim_seconds`).
        MecMode::LedgeClimb => (Some(mantle_up(mec.move_to[2] - mec.move_from[2])), true),
        MecMode::Vault if mec.wall_side == script_kind::VAULT_ONTO => {
            (Some(mantle_up(mec.move_to[2] - mec.move_from[2])), true)
        }
        MecMode::Vault => {
            let h = mec.move_mid[2] - mec.move_from[2] - crate::MecTuning::DEFAULT.vault_clearance;
            let mt = if h <= 36.0 {
                ANIM_MT_MANTLE_OVER_LOW
            } else if h <= 50.0 {
                ANIM_MT_MANTLE_OVER_MID
            } else {
                ANIM_MT_MANTLE_OVER_HIGH
            };
            (Some(mt), true)
        }
        MecMode::Air => (None, false),
    }
}

impl MecMoveState {
    /// Camera roll (degrees, + = roll right) this state asks for; the view
    /// smooths toward it.
    #[must_use]
    pub fn camera_roll(&self, tuning: &crate::MecTuning) -> f32 {
        if self.mode == MecMode::WallRun {
            -f32::from(self.wall_side) * tuning.wallrun_camera_roll_deg
        } else {
            0.0
        }
    }
}

/// `mp_mantle_up_N` movetype whose height is closest to `h` inches.
fn mantle_up(h: f32) -> u8 {
    let mut best = 0;
    for (i, mh) in MANTLE_UP_HEIGHTS.iter().enumerate() {
        if (mh - h).abs() < (MANTLE_UP_HEIGHTS[best] - h).abs() {
            best = i;
        }
    }
    ANIM_MT_MANTLE_UP_57 + best as u8
}

impl MecMoveState {
    /// Length (s) the third-person legs clip should be stretched to: the
    /// scripted move's decoded duration (vault / ledge climb), else `None`.
    #[must_use]
    pub fn scripted_anim_seconds(&self) -> Option<f32> {
        matches!(self.mode, MecMode::Vault | MecMode::LedgeClimb)
            .then(|| self.move_ms as f32 / 1000.0)
            .filter(|s| *s > 0.0)
    }

    /// The move the camera / body effects follow, with its duration.
    #[must_use]
    pub fn camera_clip(&self, elapsed: f32, tuning: &crate::MecTuning) -> crate::camera::MoveClip {
        let duration = match self.mode {
            MecMode::Roll => tuning.roll_time,
            MecMode::HardLanding => match self.wall_side {
                script_kind::STUMBLE => tuning.stumble_time,
                script_kind::FAIL_MEDIUM => tuning.fail_medium_time,
                _ => tuning.fail_time,
            },
            _ => self.move_ms as f32 / 1000.0,
        };
        crate::camera::MoveClip {
            mode: self.mode,
            kind: self.wall_side,
            duration,
            elapsed,
        }
    }
}

/// Counts the traces one move runs (slow-move diagnostics in the sim).
struct CountingBackend<'a, C> {
    inner: &'a C,
    traces: core::cell::Cell<u32>,
}

impl<C: CollisionBackend> CollisionBackend for CountingBackend<'_, C> {
    fn trace(&self, input: movement_iw4::GroundTraceInput) -> trace_iw4::Trace {
        self.traces.set(self.traces.get().saturating_add(1));
        self.inner.trace(input)
    }

    fn touch_entity(&self, entity: i32) {
        self.inner.touch_entity(entity);
    }
}
