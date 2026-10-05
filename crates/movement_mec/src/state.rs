use movement_iw4::ViewAngleClamp;

use crate::tuning::MecTuning;

/// Top-level locomotion mode. Stored as `u8` in snapshots (`MecMode as u8`).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MecMode {
    #[default]
    Ground = 0,
    Air = 1,
    Slide = 2,
    WallRun = 3,
    WallClimb = 4,
    LedgeClimb = 5,
    Vault = 6,
    Roll = 7,
    HardLanding = 8,
}

impl MecMode {
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Air,
            2 => Self::Slide,
            3 => Self::WallRun,
            4 => Self::WallClimb,
            5 => Self::LedgeClimb,
            6 => Self::Vault,
            7 => Self::Roll,
            8 => Self::HardLanding,
            _ => Self::Ground,
        }
    }

    /// Modes whose motion is a scripted path (kinematic, no input).
    #[must_use]
    pub const fn is_scripted(self) -> bool {
        matches!(self, Self::LedgeClimb | Self::Vault)
    }
}

/// `air_flags` bits: things allowed once per airtime, cleared on landing.
pub mod air_flags {
    pub const USED_WALLCLIMB: u8 = 0x1;
    pub const USED_WALLRUN: u8 = 0x2;
    /// Quickturn happened during the current wallclimb: the next jump launches
    /// away from the wall.
    pub const TURNED_ON_WALL: u8 = 0x4;
    /// A second wallrun was used (only with `wallruns_per_air` 2).
    pub const USED_WALLRUN_2: u8 = 0x8;
    /// Wall moves (wallrun / wallclimb, any type) this airtime: 2-bit count.
    pub const WALL_MOVES_SHIFT: u8 = 4;
    pub const WALL_MOVES_MASK: u8 = 0x30;
    /// A second wallclimb was used (only with `wallclimbs_per_air` 2).
    pub const USED_WALLCLIMB_2: u8 = 0x40;

    #[must_use]
    pub const fn wallclimbs(flags: u8) -> u8 {
        (flags & USED_WALLCLIMB != 0) as u8 + (flags & USED_WALLCLIMB_2 != 0) as u8
    }

    #[must_use]
    pub const fn wall_moves(flags: u8) -> u8 {
        (flags & WALL_MOVES_MASK) >> WALL_MOVES_SHIFT
    }

    #[must_use]
    pub const fn wallruns(flags: u8) -> u8 {
        (flags & USED_WALLRUN != 0) as u8 + (flags & USED_WALLRUN_2 != 0) as u8
    }

    #[must_use]
    pub const fn add_wall_move(flags: u8) -> u8 {
        let n = wall_moves(flags);
        let n = if n < 3 { n + 1 } else { 3 };
        (flags & !WALL_MOVES_MASK) | (n << WALL_MOVES_SHIFT)
    }
}

/// What `MecMoveState::wall_side` holds while a scripted / landing mode runs
/// (it is the wall side only in `WallRun`).
pub mod script_kind {
    /// Vault over, short (VaultOverFast, 0.60 s).
    pub const VAULT_OVER: i8 = 1;
    /// Vault over a deep obstacle (VaultOverFastLong, 0.75 s).
    pub const VAULT_OVER_LONG: i8 = 2;
    /// Vault onto (VaultOnto curve).
    pub const VAULT_ONTO: i8 = 3;
    /// Ledge climb from a wall / jump (VaultOntoHigh curve, heave-up timing).
    pub const LEDGE_HIGH: i8 = 4;
    /// Ledge climb of a ledge no higher than a vault (VaultOnto curve).
    pub const LEDGE_LOW: i8 = 5;
    /// HardLanding tiers.
    pub const STUMBLE: i8 = 6;
    pub const FAIL_MEDIUM: i8 = 7;
    pub const FAIL: i8 = 8;
    pub const DEATH: i8 = 9;
}

/// Everything movement_mec needs to carry between ticks. It must live next to
/// `PlayerState` in the sim and in the snapshot (prediction replays depend on
/// it). Plain `Copy` data; no pointers, no allocation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MecMoveState {
    pub mode: MecMode,
    /// Milliseconds spent in `mode`.
    pub mode_ms: i32,
    /// 0..=1 position on the sprint speed curve (time / `run_curve_time`).
    pub momentum: f32,
    /// Wall normal of the current wallrun / wallclimb (unit, horizontal).
    /// Vault / LedgeClimb: the velocity at the start of the move (the path
    /// blends from it). Roll: `[entry speed in/s, 0, 0]`.
    pub wall_normal: [f32; 3],
    /// `WallRun`: -1 wall on the left, +1 on the right. Vault / LedgeClimb /
    /// HardLanding: a [`script_kind`] value. Otherwise 0.
    pub wall_side: i8,
    /// Last wall used this airtime (no re-attaching to the same wall).
    pub last_wall_normal: [f32; 3],
    pub air_flags: u8,
    /// Highest origin z since leaving the ground (landing animation / camera dip).
    pub fall_start_z: f32,
    /// Milliseconds since crouch was last pressed (saturates).
    pub crouch_press_ms: i32,
    /// Milliseconds since jump was pressed and not yet used by a move
    /// (saturates at [`CROUCH_NEVER_MS`]; set to it once a move takes the
    /// press). Buffers a press for the springboard (`springboard_sighting_time`)
    /// and for the vault's springboard branch.
    pub jump_press_ms: i32,
    /// Air / WallRun / WallClimb: the jump start (where the body last left
    /// the ground or jumped; the landing drop and the wall-move gates measure
    /// from it). Vault / LedgeClimb: the scripted move's start.
    pub move_from: [f32; 3],
    /// Vault / LedgeClimb: the point above the start that clears the lip.
    /// WallRun: `[gap to the wall at entry, _, entry z]` (the align pull and the
    /// arc's base). WallClimb: `[_, _, entry z]`.
    pub move_mid: [f32; 3],
    /// Vault / LedgeClimb: the end of the path.
    pub move_to: [f32; 3],
    /// Duration (ms) of the timed mode: scripted path, wallrun, wallclimb,
    /// roll move-out, hard-landing control return.
    pub move_ms: i32,
    /// Vault / LedgeClimb: velocity restored at the end. Roll: unit direction.
    pub exit_velocity: [f32; 3],
    /// Quickturn yaw still to apply (degrees, signed).
    pub turn_remaining: f32,
    /// Buttons of the previous command (edge detection).
    pub old_buttons: u32,
    /// Legs tucked (coil): hull bottom raised by `coil_lift`.
    pub coil: bool,
    /// Hull is crouched (slide / roll / crouch-walk).
    pub crouched: bool,
}

impl MecMoveState {
    pub const SPAWN: Self = Self {
        mode: MecMode::Ground,
        mode_ms: 0,
        momentum: 0.0,
        wall_normal: [0.0; 3],
        wall_side: 0,
        last_wall_normal: [0.0; 3],
        air_flags: 0,
        fall_start_z: 0.0,
        crouch_press_ms: CROUCH_NEVER_MS,
        jump_press_ms: CROUCH_NEVER_MS,
        move_from: [0.0; 3],
        move_mid: [0.0; 3],
        move_to: [0.0; 3],
        move_ms: 0,
        exit_velocity: [0.0; 3],
        turn_remaining: 0.0,
        old_buttons: 0,
        coil: false,
        crouched: false,
    };
}

impl MecMoveState {
    /// Catalyst state name for logs and traces: the mode, except that a
    /// wallclimb turned by a quickturn reads `WallClimb180` (the node it is).
    #[must_use]
    pub const fn mode_name(&self) -> &'static str {
        match self.mode {
            MecMode::WallClimb if self.air_flags & air_flags::TURNED_ON_WALL != 0 => "WallClimb180",
            MecMode::Ground => "Ground",
            MecMode::Air => "Air",
            MecMode::Slide => "Slide",
            MecMode::WallRun => "WallRun",
            MecMode::WallClimb => "WallClimb",
            MecMode::LedgeClimb => "LedgeClimb",
            MecMode::Vault => "Vault",
            MecMode::Roll => "Roll",
            MecMode::HardLanding => "HardLanding",
        }
    }
}

impl Default for MecMoveState {
    fn default() -> Self {
        Self::SPAWN
    }
}

pub const CROUCH_NEVER_MS: i32 = 1 << 20;

/// Hull and trace setup. Defaults are IW4's player box so hitboxes, linking and
/// netcode stay unchanged (Catalyst capsule: r 0.3 m = 11.8 in, h 1.8 m = 70.9 in).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MecBounds {
    pub half_width: f32,
    pub stand_height: f32,
    pub crouch_height: f32,
    pub stand_view_height: f32,
    pub crouch_view_height: f32,
    pub tracemask: u32,
}

impl MecBounds {
    /// IW4 player hull (±15, 0..70 / 50), view 60 / 40, MASK_PLAYERSOLID.
    pub const IW4: Self = Self {
        half_width: 15.0,
        stand_height: 70.0,
        crouch_height: 50.0,
        stand_view_height: 60.0,
        crouch_view_height: 40.0,
        tracemask: 0x0281_0011,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MecContext {
    /// Command duration (`cmd.server_time - ps.command_time`), clamped to 0..=200.
    pub msec: i32,
    pub tuning: MecTuning,
    pub bounds: MecBounds,
    pub view_clamp: ViewAngleClamp,
    /// Button bit that requests a 180 quickturn (IW4 has no such button; the
    /// input layer must map one, see the integration note).
    pub quickturn_button: u32,
    /// Multiplier on wished ground speed (weapon move speed scale, ADS slow-down).
    /// Momentum follows actual speed, so a scale below 1 (ADS) bleeds it.
    pub speed_scale: f32,
}

impl MecContext {
    pub const QUICKTURN_BUTTON_DEFAULT: u32 = 1 << 22;

    #[must_use]
    pub const fn new(msec: i32) -> Self {
        Self {
            msec,
            tuning: MecTuning::DEFAULT,
            bounds: MecBounds::IW4,
            view_clamp: ViewAngleClamp {
                pitch_up: 85.0,
                pitch_down: 85.0,
                unclamped_pitch_bit: false,
            },
            quickturn_button: Self::QUICKTURN_BUTTON_DEFAULT,
            speed_scale: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MecEvent {
    Jump,
    Springboard,
    WallJump,
    WallRunStart {
        side: i8,
    },
    WallRunEnd,
    WallClimbStart,
    WallClimbEnd,
    LedgeClimbStart,
    VaultStart {
        onto: bool,
    },
    SlideStart,
    SlideEnd,
    CoilStart,
    QuickTurn,
    /// Ordinary touchdown; fall height in inches.
    Land {
        fall_height: f32,
    },
    Roll {
        fall_height: f32,
    },
    HardLanding {
        fall_height: f32,
    },
}

pub const MAX_EVENTS: usize = 8;

/// What a weapon may do this tick (feed into the sprint/weapon interlock).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeaponGates {
    pub allow_fire: bool,
    pub allow_ads: bool,
    /// Weapon should be lowered / holstered (hands busy).
    pub lowered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MecMoveResult {
    pub events: [Option<MecEvent>; MAX_EVENTS],
    /// Hull used at the end of the move (for `link_player_area`).
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub view_height: f32,
    /// Suggested camera roll in degrees (+ = roll right).
    pub camera_roll: f32,
    pub gates: WeaponGates,
    /// Damage to apply from the landing (0 when none).
    pub fall_damage: i32,
    /// 0..=1 momentum shield ("Focus") strength from current horizontal speed.
    pub shield: f32,
    /// On walkable ground this tick.
    pub walking: bool,
}

impl MecMoveResult {
    pub(crate) fn push(&mut self, event: MecEvent) {
        if let Some(slot) = self.events.iter_mut().find(|e| e.is_none()) {
            *slot = Some(event);
        }
    }

    pub fn iter_events(&self) -> impl Iterator<Item = MecEvent> + '_ {
        self.events.iter().flatten().copied()
    }
}
