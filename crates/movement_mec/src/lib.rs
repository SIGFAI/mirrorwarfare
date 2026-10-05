//! Mirror's Edge Catalyst-feel player movement for Mirrorwarfare.
//!
//! A deterministic state machine (ground / air / slide / wallrun / wallclimb /
//! ledge climb / vault / roll / hard landing, plus coil and quickturn) that
//! runs one usercmd at a time over [`movement_iw4::CollisionBackend`], in IW4
//! units (inches, Z-up). Entry point: [`mec_pmove`]. Per-player state that must
//! be carried in the sim and snapshot: [`MecMoveState`]. Tuning and its
//! provenance: [`MecTuning`] (numbers from Faith's decoded ANT graph).
#![no_std]
#![forbid(unsafe_code)]

mod body;
mod camera;
mod iw4;
mod pmove;
mod probes;
pub mod rootmotion;
mod state;
mod tuning;
mod vec;

pub use body::Hull;
pub use camera::{MecCameraMotion, MoveClip, camera_motion};
pub use iw4::{MecStep, mec_pmove_iw4};
pub use pmove::{mec_pmove, script_curve, wallclimb_arc, wallrun_arc};
pub use state::{
    CROUCH_NEVER_MS, MAX_EVENTS, MecBounds, MecContext, MecEvent, MecMode, MecMoveResult,
    MecMoveState, WeaponGates, air_flags, script_kind,
};
pub use tuning::{
    JUMP_TABLE, JumpRow, METRE, MecTuning, MecUnlocks, SLIDE_CURVE, SPRINT_CURVE, TICK, curve_at,
    curve_inverse,
};

#[cfg(test)]
mod tests;
