//! Tuning, in IW4 units (inches, seconds, degrees; Z-up). Every value carries
//! its provenance tag. The numbers come from Faith's ANT state graph
//! (`context/artifacts/2026-10-04-mec-ant/README.md` §3 and §6; ANT times are
//! ticks of 1/60 s) and from EBX
//! (`context/artifacts/2026-10-04-mec-movement/README.md`).
//!
//! * `DATA`     — the number is in Catalyst ANT/EBX with a clear meaning.
//! * `DATA/INF` — the number is in the data, its meaning is a reading.
//! * `INFERRED` — derived from data numbers.
//! * `GUESS`    — not in the decoded data; picked for feel / FFA play.
//! * `FFA`      — deliberate deviation from the data for MW2 free-for-all.

/// Inches per metre (1 m = 39.37 IW4 units).
pub const METRE: f32 = 39.37;

/// ANT tick (1/60 s) in seconds.
pub const TICK: f32 = 1.0 / 60.0;

/// `WalkSetting.Sprint.SpeedCurve` (DATA): (t / MaximumTime, v / MaximumSpeed).
/// MaximumTime 4.0 s, MaximumSpeed 7.2 m/s: 2.0 m/s at 0, 4.0 @0.25 s,
/// 5.0 @0.5 s, 5.9 @0.75 s, 6.7 @1.0 s, 7.0 @1.25 s, 7.13 @1.5 s, 7.2 @3 s.
pub const SPRINT_CURVE: [(f32, f32); 9] = [
    (0.0, 0.279),
    (0.061, 0.556),
    (0.124, 0.694),
    (0.186, 0.816),
    (0.249, 0.935),
    (0.312, 0.977),
    (0.376, 0.99),
    (0.749, 1.0),
    (1.0, 1.0),
];

/// `WalkSetting.Slide.SpeedCurve` (DATA): (t / MaximumTime, v / MaximumSpeed),
/// MaximumTime 3.0 s, MaximumSpeed 10 m/s.
pub const SLIDE_CURVE: [(f32, f32); 3] = [(0.0, 1.0), (0.244, 0.691), (1.0, 0.0)];

/// One row of `MovementSystem.JumpDatabase` (DATA): entries apply from
/// `min_speed` (horizontal entry speed) up to the next row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JumpRow {
    pub min_speed: f32,
    /// Horizontal speed set by the jump driver (`FallingDriverSettings.ForwardSpeed`).
    pub forward_speed: f32,
    /// Apex above take-off (`JumpHeight`).
    pub height: f32,
}

/// Jump drivers by entry speed: JumpStill (803), JumpVerySlow (795),
/// JumpSlow (802), JumpMedium (805), JumpFast (806).
pub const JUMP_TABLE: [JumpRow; 5] = [
    JumpRow {
        min_speed: 0.0,
        forward_speed: 0.0,
        height: 1.1 * METRE,
    },
    JumpRow {
        min_speed: 0.5 * METRE,
        forward_speed: 2.28 * METRE,
        height: 1.1 * METRE,
    },
    JumpRow {
        min_speed: 2.0 * METRE,
        forward_speed: 4.12 * METRE,
        height: 1.1 * METRE,
    },
    JumpRow {
        min_speed: 3.6 * METRE,
        forward_speed: 5.96 * METRE,
        height: 1.1 * METRE,
    },
    JumpRow {
        min_speed: 6.5 * METRE,
        forward_speed: 8.04 * METRE,
        height: 1.2 * METRE,
    },
];

/// Piecewise-linear lookup in a (x, y) table with ascending x.
#[must_use]
pub fn curve_at(curve: &[(f32, f32)], x: f32) -> f32 {
    let (x0, y0) = curve[0];
    if x <= x0 {
        return y0;
    }
    for w in curve.windows(2) {
        let (a, b) = (w[0], w[1]);
        if x <= b.0 {
            let f = if b.0 > a.0 {
                (x - a.0) / (b.0 - a.0)
            } else {
                1.0
            };
            return a.1 + (b.1 - a.1) * f;
        }
    }
    curve[curve.len() - 1].1
}

/// Smallest x where a monotone (non-decreasing or non-increasing) curve
/// reaches `y`; clamps to the curve ends.
#[must_use]
pub fn curve_inverse(curve: &[(f32, f32)], y: f32) -> f32 {
    let rising = curve[curve.len() - 1].1 >= curve[0].1;
    let before = |v: f32| if rising { v < y } else { v > y };
    if !before(curve[0].1) {
        return curve[0].0;
    }
    for w in curve.windows(2) {
        let (a, b) = (w[0], w[1]);
        if !before(b.1) {
            let dy = b.1 - a.1;
            let f = if libm::fabsf(dy) > 1.0e-6 {
                (y - a.1) / dy
            } else {
                1.0
            };
            return a.0 + (b.0 - a.0) * f;
        }
    }
    curve[curve.len() - 1].0
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MecTuning {
    // --- body / world -------------------------------------------------------
    /// DATA (FloatAsset Character.GravityY −19.64 m/s²).
    pub gravity: f32,
    /// DATA (CollisionDriver CustomCollisionStepUpHeight 0.24 m).
    pub step_height: f32,
    /// Walkable ground: minimum normal z (IW4 MIN_WALK_NORMAL 0.7).
    pub min_walk_normal: f32,

    // --- ground speed ----------------------------------------------------------
    /// DATA (WalkSetting.Walk MaximumSpeed 2.0 m/s): walk / non-forward speed.
    pub walk_speed: f32,
    /// DATA (WalkSetting.Crouch MaximumSpeed 2.0 m/s).
    pub crouch_speed: f32,
    /// DATA (WalkSetting.Sprint MaximumSpeed 7.2 m/s): top of [`SPRINT_CURVE`].
    pub run_max_speed: f32,
    /// DATA (WalkSetting.Sprint MaximumTime 4.0 s): time axis of [`SPRINT_CURVE`].
    pub run_curve_time: f32,
    /// GUESS: acceleration up to the curve's start speed and along it (the
    /// curve's steepest part is 8 m/s², so this never limits it).
    pub ground_accel: f32,
    /// INFERRED (Loco_Run_Stop_Fwd: 0.64 m in 0.83 s from 7.2 m/s ⇒ ~9 m/s²; kept a bit firmer).
    pub ground_decel: f32,
    /// DATA/INF (WalkSetting.Sprint AboveMaxDecel 10): deceleration above the curve target.
    pub above_max_decel: f32,
    /// GUESS: how fast velocity heading follows input heading on the ground (deg/s), speed-preserving.
    pub turn_rate_deg: f32,

    // --- air ------------------------------------------------------------------
    /// DATA (JumpStill/Slow/Medium JumpHeight 1.1 m): standard jump apex. See [`JUMP_TABLE`].
    pub jump_height: f32,
    /// GUESS: air acceleration (cannot raise horizontal speed above entry speed).
    pub air_accel: f32,
    /// DATA (CoilDriver CoilHeight 0.64 m): how far coil raises the bottom of the hull.
    pub coil_lift: f32,

    // --- springboard (jump-assisted jump off a low obstacle) ------------------
    /// DATA (SpringboardDriverSettings.MinimumForwardSpeed 5.5 m/s).
    pub springboard_min_speed: f32,
    /// DATA (SpringboardDriverSettings.MaximumForwardSpeed 7.0 m/s).
    pub springboard_max_speed: f32,
    /// DATA (SpringboardDriverSettings.MinimumJumpHeight 1.7 m).
    pub springboard_min_height: f32,
    /// DATA (SpringboardDriverSettings.MaximumJumpHeight 2.8 m).
    pub springboard_max_height: f32,
    /// DATA (SpringboardDriverSettings.AllowedJumpAssistDistance 1.6 m).
    pub springboard_assist_dist: f32,
    /// DATA (Springboard.IsAllowed: obstacle top 0.9–1.5 m above take-off).
    pub springboard_obstacle_min: f32,
    pub springboard_obstacle_max: f32,
    /// DATA (Springboard.IsAllowed: obstacle sighted < 0.5 s before, `Springboard.Timestamp`):
    /// a jump pressed this long before reaching a springboard obstacle is held
    /// (buffered) and becomes the springboard instead of a jump or a vault.
    pub springboard_sighting_time: f32,
    /// DATA (Vault sequences, `SpringboardBranch` BufferedBranchOutPointTag, buffer from
    /// tick 0): ticks of the vault in which a buffered jump branches to Springboard.
    /// VaultOverFastSeq 16–36 t, VaultOverFastLongSeq 23–45 t, VaultOntoSeq 20–30 t.
    pub springboard_branch_over: (f32, f32),
    pub springboard_branch_over_long: (f32, f32),
    pub springboard_branch_onto: (f32, f32),

    // --- wallrun --------------------------------------------------------------
    /// DATA (WallrunDriver WallrunTime 80 t = 1.333 s).
    pub wallrun_time: f32,
    /// DATA (WallrunDriver SecondWallrunTime 64 t = 1.067 s): a second wallrun in one airtime.
    pub wallrun_second_time: f32,
    /// DATA (WallrunDriver WallrunApexTime 32 t = 0.533 s).
    pub wallrun_apex_time: f32,
    /// DATA (WallrunDriver WallrunHeight 1.2 m): rise to apex.
    pub wallrun_height: f32,
    /// INFERRED (WallRunLeft/Right root: rise 0.70 m, end −0.41 m ⇒ end/rise −0.59,
    /// scaled to the 1.2 m driver rise): height relative to entry when the wallrun ends.
    pub wallrun_end_height: f32,
    /// DATA (Min/MaxWallrunLength 4–10 m over the wallrun time): along-wall speed clamp.
    pub wallrun_min_length: f32,
    pub wallrun_max_length: f32,
    /// DATA (IsWallrunAllowed: falling speed > −3 m/s).
    pub wallrun_max_fall_speed: f32,
    /// DATA (IsWallrunAllowed: XZ jump start → wall < 8 m, < 4 m once falling).
    pub wallrun_max_jump_dist: f32,
    pub wallrun_max_jump_dist_falling: f32,
    /// DATA (wallruns < 1 per airtime; 2 with Unlock.ExtendedWallrun, see [`MecUnlocks`]).
    pub wallruns_per_air: u8,
    /// DATA (consecutive wall moves of any type < 2 per airtime; 3 extended).
    pub wall_moves_per_air: u8,
    /// DATA (IsWallClimbAllowed: wallclimbs < 1 per airtime; 2 extended).
    pub wallclimbs_per_air: u8,
    /// DATA (WallrunDriver AlignTime 16 t = 0.267 s): the body is pulled onto
    /// the wall over this time, so a wall this far ahead (at the current speed
    /// towards it) can be attached to.
    pub wallrun_align_time: f32,
    /// DATA (new wall: first in the airtime or yaw differs > 45° from the last one).
    pub new_wall_min_angle_deg: f32,
    /// GUESS: probe distance past the hull side to find the wall.
    pub wallrun_attach_dist: f32,
    /// GUESS: max angle (deg) between velocity and the wall plane at entry (C++ obstacle analysis, not data).
    pub wallrun_max_entry_angle_deg: f32,
    /// GUESS: off-wall push of a wall jump (WallrunJumpDriver ForwardSpeed 0, look-dir 180°).
    /// INFERRED (UseLookDirAngleLimit 180°, CompensateForHighSpeed false): looking
    /// away from the wall sends the jump that way at the wallrun speed; this
    /// push is the minimum off-wall speed either way.
    pub walljump_push: f32,
    /// GUESS: camera roll while wallrunning (deg). No roll in the data
    /// (FPSCameraData.TurnEffect.MaxRollAngle 0, CameraConstraintTag yaw only).
    pub wallrun_camera_roll_deg: f32,

    // --- wallclimb ------------------------------------------------------------
    /// DATA (WallclimbDriver WallrunHeight 2.6 m): rise to apex.
    pub wallclimb_height: f32,
    /// DATA (WallclimbDriver WallrunApexTime 60 t = 1.0 s).
    pub wallclimb_apex_time: f32,
    /// DATA (WallclimbDriver WallrunTime 100 t = 1.667 s): whole move, on the wall.
    pub wallclimb_time: f32,
    /// DATA/INF (WallclimbDriver WallrunPostApexHeight 1.3 m): drop after the apex until release.
    pub wallclimb_post_apex_drop: f32,
    /// DATA (IsWallClimbAllowed: falling speed > −5 m/s).
    pub wallclimb_max_fall_speed: f32,
    /// DATA (IsWallClimbAllowed: XZ distance to the hit < 0.8 m, from the body axis).
    pub wallclimb_max_dist: f32,
    /// DATA/INF (WallclimbDriver MaximumAngleDelta 40°): facing vs the wall's inward normal.
    pub wallclimb_max_angle_deg: f32,
    /// DATA (WallclimbJump180Driver 799: JumpHeight 1.1 m, ForwardSpeed 7.2 m/s,
    /// look-direction limit 180°): horizontal speed of the jump off the wall
    /// after a quickturn on it (WallClimb180).
    pub wallclimb_turn_jump_speed: f32,

    // --- ledge ------------------------------------------------------------------
    /// GUESS: highest ledge reachable above the feet (hands at full stretch ~2.1 m).
    pub ledge_max_reach: f32,
    /// DATA (CantVaultOrHeave: wall top ≥ 4.49 m above the jump start).
    pub ledge_max_above_jump: f32,
    /// DATA (HangHeaveUpSeq MoveOut 60 t = 1.0 s): ledge climb from a wall / hang,
    /// path from the VaultOntoHigh root-motion curve.
    pub ledge_climb_time: f32,
    /// DATA (VaultOnto root-motion clip 0.5 s): climb onto a ledge ≤ `vault_max_height`.
    pub ledge_climb_low_time: f32,

    // --- vault ------------------------------------------------------------------
    /// DATA (AllowedToVault: LedgeDistance < a·h + b + c·v/7.2): 0.2, 0.8 m, 0.6 m.
    pub vault_reach_h: f32,
    pub vault_reach_base: f32,
    pub vault_reach_speed: f32,
    /// DATA (VaultDatabase RealObstacleHeight 0–1.7 m for the vault-over rows).
    pub vault_max_height: f32,
    /// DATA (VaultOverFast BehindLength ≤ 1.4 m).
    pub vault_short_depth: f32,
    /// DATA (VaultOverFastLong BehindLength 1.4–4.0 m); deeper = vault onto.
    pub vault_long_depth: f32,
    /// DATA (VaultOver* BehindHeight ≥ 0.5 m drop behind, else vault onto).
    pub vault_onto_drop: f32,
    /// DATA (VaultOverFastSeq 36 t = 0.60 s, root 3.70 m).
    pub vault_short_time: f32,
    pub vault_short_dist: f32,
    /// DATA (VaultOverFastLongSeq 45 t = 0.75 s, root 4.82 m).
    pub vault_long_time: f32,
    pub vault_long_dist: f32,
    /// DATA (VaultController end speed 4.0–6.5 m/s).
    pub vault_exit_min_speed: f32,
    pub vault_exit_max_speed: f32,
    /// GUESS: clearance kept above the obstacle top during the vault.
    pub vault_clearance: f32,

    // --- slide ------------------------------------------------------------------
    /// DATA (AbortSlide: speed < 4.0 m/s): minimum speed to start / keep a slide.
    pub slide_min_speed: f32,
    /// DATA (WalkSetting.Slide MaximumSpeed 10 m/s): speed axis of [`SLIDE_CURVE`].
    pub slide_curve_speed: f32,
    /// DATA (WalkSetting.Slide MaximumTime 3.0 s; UnlockSlide 5.0): max slide time and curve time axis.
    pub slide_max_time: f32,

    // --- landing ----------------------------------------------------------------
    /// DATA (CanSkillRoll: drop > 1.0 m).
    pub roll_min_drop: f32,
    /// DATA (FallingLandRollSeq 70 t = 1.167 s; root 4.4 m).
    pub roll_time: f32,
    /// DATA (FallingLandRollSeq MoveOut 60 t = 1.0 s): control returns.
    pub roll_move_out: f32,
    pub roll_dist: f32,
    /// DATA (HeavyLandingHeight: drop > 2.0 m) → stumble.
    pub stumble_height: f32,
    /// DATA (FallingLandFailNoDamage 38 t = 0.63 s).
    pub stumble_time: f32,
    /// DATA (LandHardDatabase 4–6 m → FallingLandFailMedium 132 t, MoveOut 122 t).
    pub fail_medium_height: f32,
    pub fail_medium_time: f32,
    pub fail_medium_move_out: f32,
    /// DATA (LandHardDatabase 6–10 m → FallingLandFail 202 t, MoveOut 177 t).
    pub fail_height: f32,
    pub fail_time: f32,
    pub fail_move_out: f32,
    /// DATA (DeathLandingHeight: drop > 10 m; the roll does not save it).
    pub lethal_fall_height: f32,
    /// FFA: damage at the start / end of the medium fail tier (4 → 6 m).
    pub fail_medium_damage: (f32, f32),
    /// FFA: damage at the start / end of the fail tier (6 → 10 m); stays below 100.
    pub fail_damage: (f32, f32),
    /// FFA: speed allowed while stumbling (the 2–4 m tier has no damage).
    pub stumble_speed: f32,

    // --- quickturn --------------------------------------------------------------
    /// DATA (StandRunTurn180 36 t = 0.60 s).
    pub quickturn_time: f32,
    /// DATA (StandRunTurn180 MoveOut 32 t = 0.533 s): the 180° is done and input returns.
    pub quickturn_move_out: f32,

    // --- focus / momentum shield ------------------------------------------------
    /// DATA (FaithShield FloatEaseFunction InputRangeMin 5.0): speed where the shield starts.
    pub shield_min_speed: f32,
    /// DATA (FaithShield FloatEaseFunction InputRangeMax 7.0): speed where the shield is full.
    pub shield_full_speed: f32,

    // --- unlocks ----------------------------------------------------------------
    /// Which of Faith's skill-tree unlocks are on. [`MecTuning::BASE`] has none
    /// (the decoded base values above); [`MecTuning::DEFAULT`] (FFA) has all of
    /// them, applied by [`MecTuning::with_unlocks`].
    pub unlocks: MecUnlocks,
}

/// Faith's movement unlocks (`Unlock.*` GameState bools read by the ANT graph,
/// `evidence/antrefs.tsv`). FFA: everyone has the full move set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MecUnlocks {
    /// `Unlock.ExtendedWallrun` (DATA, IsWallrunAllowed / IsWallClimbAllowed):
    /// wallruns per airtime 1 → 2, wallclimbs 1 → 2, wall moves of any type 2 → 3.
    pub extended_wallrun: bool,
    /// `Unlock.LongSlide` (DATA, SlideUnlockDriver: WalkSetting.Slide MaximumTime 3.0 → 5.0 s).
    pub long_slide: bool,
    /// `Unlock.Coil` (DATA, Coil node entry): tuck the legs while rising.
    pub coil: bool,
    /// `Unlock.QuickTurn` (DATA, Turn180 node entry): 180° turn.
    pub quickturn: bool,
    /// `Unlock.Shift` (DATA, DashOut windows): the sideways dash. Not implemented
    /// in movement_mec yet; the flag has no effect.
    pub shift: bool,
    /// `Unlock.FastSkillroll` (DATA, SkillRollFast / Slide BoostOut). Not
    /// implemented yet; the flag has no effect.
    pub fast_skillroll: bool,
}

impl MecUnlocks {
    /// Nothing unlocked (Catalyst start of game).
    pub const NONE: Self = Self {
        extended_wallrun: false,
        long_slide: false,
        coil: false,
        quickturn: false,
        shift: false,
        fast_skillroll: false,
    };
    /// Everything unlocked (FFA).
    pub const ALL: Self = Self {
        extended_wallrun: true,
        long_slide: true,
        coil: true,
        quickturn: true,
        shift: true,
        fast_skillroll: true,
    };
}

/// Unlock.ExtendedWallrun values (DATA, IsWallrunAllowed / IsWallClimbAllowed ins. 280–350).
const EXTENDED_WALLRUNS: u8 = 2;
const EXTENDED_WALLCLIMBS: u8 = 2;
const EXTENDED_WALL_MOVES: u8 = 3;
/// Unlock.LongSlide (DATA, SlideUnlockDriver MaximumTime 5.0 s).
const LONG_SLIDE_TIME: f32 = 5.0;

impl MecTuning {
    /// The decoded base values, nothing unlocked.
    pub const BASE: Self = Self {
        gravity: 19.64 * METRE,
        step_height: 0.24 * METRE,
        min_walk_normal: 0.7,

        walk_speed: 2.0 * METRE,
        crouch_speed: 2.0 * METRE,
        run_max_speed: 7.2 * METRE,
        run_curve_time: 4.0,
        ground_accel: 12.0 * METRE,
        ground_decel: 12.0 * METRE,
        above_max_decel: 10.0 * METRE,
        turn_rate_deg: 240.0,

        jump_height: 1.1 * METRE,
        air_accel: 3.0 * METRE,
        coil_lift: 0.64 * METRE,

        springboard_min_speed: 5.5 * METRE,
        springboard_max_speed: 7.0 * METRE,
        springboard_min_height: 1.7 * METRE,
        springboard_max_height: 2.8 * METRE,
        springboard_assist_dist: 1.6 * METRE,
        springboard_obstacle_min: 0.9 * METRE,
        springboard_obstacle_max: 1.5 * METRE,
        springboard_sighting_time: 0.5,
        springboard_branch_over: (16.0 * TICK, 36.0 * TICK),
        springboard_branch_over_long: (23.0 * TICK, 45.0 * TICK),
        springboard_branch_onto: (20.0 * TICK, 30.0 * TICK),

        wallrun_time: 80.0 * TICK,
        wallrun_second_time: 64.0 * TICK,
        wallrun_apex_time: 32.0 * TICK,
        wallrun_height: 1.2 * METRE,
        wallrun_end_height: -0.70 * METRE,
        wallrun_min_length: 4.0 * METRE,
        wallrun_max_length: 10.0 * METRE,
        wallrun_max_fall_speed: 3.0 * METRE,
        wallrun_max_jump_dist: 8.0 * METRE,
        wallrun_max_jump_dist_falling: 4.0 * METRE,
        wallruns_per_air: 1,
        wall_moves_per_air: 2,
        wallclimbs_per_air: 1,
        wallrun_align_time: 16.0 * TICK,
        new_wall_min_angle_deg: 45.0,
        wallrun_attach_dist: 16.0,
        wallrun_max_entry_angle_deg: 50.0,
        walljump_push: 4.0 * METRE,
        wallrun_camera_roll_deg: 6.0,

        wallclimb_height: 2.6 * METRE,
        wallclimb_apex_time: 60.0 * TICK,
        wallclimb_time: 100.0 * TICK,
        wallclimb_post_apex_drop: 1.3 * METRE,
        wallclimb_max_fall_speed: 5.0 * METRE,
        wallclimb_max_dist: 0.8 * METRE,
        wallclimb_max_angle_deg: 40.0,
        wallclimb_turn_jump_speed: 7.2 * METRE,

        ledge_max_reach: 84.0,
        ledge_max_above_jump: 4.49 * METRE,
        ledge_climb_time: 60.0 * TICK,
        ledge_climb_low_time: 0.5,

        vault_reach_h: 0.2,
        vault_reach_base: 0.8 * METRE,
        vault_reach_speed: 0.6 * METRE,
        vault_max_height: 1.7 * METRE,
        vault_short_depth: 1.4 * METRE,
        vault_long_depth: 4.0 * METRE,
        vault_onto_drop: 0.5 * METRE,
        vault_short_time: 36.0 * TICK,
        vault_short_dist: 3.70 * METRE,
        vault_long_time: 45.0 * TICK,
        vault_long_dist: 4.82 * METRE,
        vault_exit_min_speed: 4.0 * METRE,
        vault_exit_max_speed: 6.5 * METRE,
        vault_clearance: 6.0,

        slide_min_speed: 4.0 * METRE,
        slide_curve_speed: 10.0 * METRE,
        slide_max_time: 3.0,

        roll_min_drop: 1.0 * METRE,
        roll_time: 70.0 * TICK,
        roll_move_out: 60.0 * TICK,
        roll_dist: 4.4 * METRE,
        stumble_height: 2.0 * METRE,
        stumble_time: 38.0 * TICK,
        fail_medium_height: 4.0 * METRE,
        fail_medium_time: 132.0 * TICK,
        fail_medium_move_out: 122.0 * TICK,
        fail_height: 6.0 * METRE,
        fail_time: 202.0 * TICK,
        fail_move_out: 177.0 * TICK,
        lethal_fall_height: 10.0 * METRE,
        fail_medium_damage: (20.0, 35.0),
        fail_damage: (35.0, 75.0),
        stumble_speed: 2.0 * METRE,

        quickturn_time: 36.0 * TICK,
        quickturn_move_out: 32.0 * TICK,

        shield_min_speed: 5.0 * METRE,
        shield_full_speed: 7.0 * METRE,

        unlocks: MecUnlocks::NONE,
    };

    /// FFA default: the base values with every unlock on (2 wallruns / 2
    /// wallclimbs / 3 wall moves per airtime, 5 s slide, coil, quickturn).
    pub const DEFAULT: Self = Self::BASE.with_unlocks(MecUnlocks::ALL);

    /// The base values with `u` applied on top.
    #[must_use]
    pub const fn with_unlocks(self, u: MecUnlocks) -> Self {
        let mut t = self;
        t.unlocks = u;
        if u.extended_wallrun {
            t.wallruns_per_air = EXTENDED_WALLRUNS;
            t.wallclimbs_per_air = EXTENDED_WALLCLIMBS;
            t.wall_moves_per_air = EXTENDED_WALL_MOVES;
        }
        if u.long_slide {
            t.slide_max_time = LONG_SLIDE_TIME;
        }
        t
    }

    /// Initial upward speed for a jump that peaks `height` above take-off.
    #[must_use]
    pub fn jump_velocity(&self, height: f32) -> f32 {
        libm::sqrtf(2.0 * self.gravity * height)
    }

    /// Jump driver row for a horizontal entry speed.
    #[must_use]
    pub fn jump_row(&self, entry_speed: f32) -> JumpRow {
        let mut row = JUMP_TABLE[0];
        for r in JUMP_TABLE {
            if entry_speed >= r.min_speed {
                row = r;
            }
        }
        row
    }

    /// Sprint speed after `t` seconds on the run curve.
    #[must_use]
    pub fn run_speed_at(&self, t: f32) -> f32 {
        self.run_max_speed * curve_at(&SPRINT_CURVE, t / self.run_curve_time)
    }

    /// Time on the run curve that `speed` corresponds to.
    #[must_use]
    pub fn run_time_for(&self, speed: f32) -> f32 {
        self.run_curve_time * curve_inverse(&SPRINT_CURVE, speed / self.run_max_speed)
    }

    /// Run-curve start speed (2.0 m/s).
    #[must_use]
    pub fn run_start_speed(&self) -> f32 {
        self.run_speed_at(0.0)
    }

    /// Slide speed after `t` seconds on the slide curve.
    #[must_use]
    pub fn slide_speed_at(&self, t: f32) -> f32 {
        self.slide_curve_speed * curve_at(&SLIDE_CURVE, t / self.slide_max_time)
    }

    /// Time on the slide curve that `speed` corresponds to.
    #[must_use]
    pub fn slide_time_for(&self, speed: f32) -> f32 {
        self.slide_max_time * curve_inverse(&SLIDE_CURVE, speed / self.slide_curve_speed)
    }

    /// AllowedToVault reach (from the body axis) for an obstacle `h` high at `speed`.
    #[must_use]
    pub fn vault_reach(&self, h: f32, speed: f32) -> f32 {
        self.vault_reach_h * h
            + self.vault_reach_base
            + self.vault_reach_speed * (speed / self.run_max_speed).min(1.5)
    }
}

impl Default for MecTuning {
    fn default() -> Self {
        Self::DEFAULT
    }
}
