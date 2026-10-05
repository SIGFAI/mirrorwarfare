//! World queries that decide which parkour move is available. All of them are
//! box traces through `CollisionBackend`, so they work on any map the sim can
//! trace (IW4 clipmaps today, Catalyst-derived collision later).

use movement_iw4::CollisionBackend;

use crate::body::{Hull, fits, reached, trace};
use crate::state::script_kind;
use crate::tuning::MecTuning;
use crate::vec::{V3, add, dot, mad, scale};

/// Small probe box used for wall/ledge feelers (hands, not body).
const FEELER: Hull = Hull {
    mins: [-4.0, -4.0, -4.0],
    maxs: [4.0, 4.0, 4.0],
    mask: 0,
};

/// Height above the feet that wall feelers are cast from (about chest).
const FEELER_Z: f32 = 40.0;

/// Max |normal.z| for a surface to count as a wall.
const WALL_MAX_NZ: f32 = 0.3;

fn feeler(mask: u32) -> Hull {
    Hull { mask, ..FEELER }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallHit {
    pub normal: V3,
    /// Distance from the hull centre axis to the wall along the probe.
    pub dist: f32,
}

/// Cast a feeler from the hull's centre at chest height along `dir` (unit,
/// horizontal) out to `half_width + reach`. Returns a near-vertical wall.
pub fn wall_along<C: CollisionBackend>(
    c: &C,
    origin: V3,
    dir: V3,
    half_width: f32,
    reach: f32,
    mask: u32,
) -> Option<WallHit> {
    let start = [origin[0], origin[1], origin[2] + FEELER_Z];
    let range = half_width + reach;
    let end = mad(start, dir, range);
    let t = trace(c, start, end, feeler(mask));
    if t.startsolid != 0 || t.fraction >= 1.0 || libm::fabsf(t.normal[2]) > WALL_MAX_NZ {
        return None;
    }
    let n = [t.normal[0], t.normal[1], 0.0];
    let nl = libm::sqrtf(n[0] * n[0] + n[1] * n[1]);
    if nl < 1.0e-3 {
        return None;
    }
    Some(WallHit {
        normal: scale(n, 1.0 / nl),
        dist: range * t.fraction + FEELER.maxs[0],
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ledge {
    /// Point straight above the start, high enough to clear the ledge lip.
    pub mid: V3,
    /// Standing position on top of the ledge.
    pub target: V3,
    pub top_z: f32,
}

#[allow(clippy::too_many_arguments)]
/// A ledge in front (`dir`, unit horizontal) whose top is between
/// `min_h` and `max_h` above the feet, with standing room on top and a clear
/// path up and over. `wall_dist` is how far the front face is from the axis.
pub fn ledge_ahead<C: CollisionBackend>(
    c: &C,
    origin: V3,
    dir: V3,
    wall_dist: f32,
    min_h: f32,
    max_h: f32,
    stand: Hull,
    half_width: f32,
) -> Option<Ledge> {
    // Probe down onto the top, just past the front face.
    let probe_xy = mad(origin, dir, wall_dist + 10.0);
    // Start the feeler wholly above `max_h` (its bottom is FEELER.mins below
    // the centre), so tops right up to the limit are found.
    let top_start = [
        probe_xy[0],
        probe_xy[1],
        origin[2] + max_h + 2.0 - FEELER.mins[2],
    ];
    let top_end = [probe_xy[0], probe_xy[1], origin[2] + min_h];
    let down = trace(c, top_start, top_end, feeler(stand.mask));
    if down.startsolid != 0 || down.fraction >= 1.0 || down.normal[2] < 0.7 {
        return None;
    }
    let top_z = down.endpos[2] + FEELER.mins[2];
    if top_z - origin[2] > max_h {
        return None;
    }
    // Stand on top, with the hull fully past the face.
    let tgt_xy = mad(origin, dir, wall_dist + half_width + 1.0);
    let target = [tgt_xy[0], tgt_xy[1], top_z + 0.25];
    if !fits(c, target, stand) {
        return None;
    }
    // Straight up to clear the lip.
    let mid = [origin[0], origin[1], top_z + 0.25];
    let up = trace(c, origin, mid, stand);
    if up.startsolid != 0 || up.fraction < 1.0 {
        return None;
    }
    // Then over onto the top.
    let over = trace(c, mid, target, stand);
    if over.fraction < 1.0 {
        return None;
    }
    Some(Ledge { mid, target, top_z })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vault {
    pub mid: V3,
    pub target: V3,
    /// Nothing to drop off behind (a platform): vault onto it and stay on top.
    pub onto: bool,
    /// [`crate::state::script_kind`] `VAULT_OVER` / `VAULT_OVER_LONG` / `VAULT_ONTO`.
    pub kind: i8,
    pub top_z: f32,
    /// Distance from the axis to the obstacle's front face.
    pub wall_dist: f32,
    /// Obstacle depth (front face to far edge) when vaulting over.
    pub depth: f32,
}

/// A vaultable obstacle in front: front face closer than the AllowedToVault
/// reach `0.2·h + 0.8 m + 0.6 m·v/7.2` (from the body axis), top between
/// `step_height` and `vault_max_height` above the feet. Depth to the far edge
/// (a drop of at least `vault_onto_drop`) picks over / over-long / onto.
/// `reach` replaces the AllowedToVault reach (springboard assist distance).
pub fn vault_ahead<C: CollisionBackend>(
    c: &C,
    origin: V3,
    dir: V3,
    speed: f32,
    reach: Option<f32>,
    stand: Hull,
    half_width: f32,
    t: &MecTuning,
) -> Option<Vault> {
    let max_reach = reach.unwrap_or_else(|| t.vault_reach(t.vault_max_height, speed));
    // Front face, at knee height so steps don't count.
    let knee = [origin[0], origin[1], origin[2] + t.step_height + 1.0];
    let knee_end = mad(knee, dir, (max_reach - half_width).max(1.0));
    let shin = Hull {
        mins: [-half_width, -half_width, 0.0],
        maxs: [half_width, half_width, 1.0],
        mask: stand.mask,
    };
    let face = trace(c, knee, knee_end, shin);
    if face.startsolid != 0 || face.fraction >= 1.0 || libm::fabsf(face.normal[2]) > WALL_MAX_NZ {
        return None;
    }
    if dot([face.normal[0], face.normal[1], 0.0], dir) > -0.5 {
        return None;
    }
    // A flat box may trace as a thin capsule (IW4 clip), so its fraction does
    // not give the face distance; measure it with the cubic feeler instead.
    let knee_mid = [knee[0], knee[1], knee[2] + FEELER.maxs[2]];
    let range = max_reach + FEELER.maxs[0];
    let measure = trace(c, knee_mid, mad(knee_mid, dir, range), feeler(stand.mask));
    if measure.startsolid != 0 || measure.fraction >= 1.0 {
        return None;
    }
    let wall_dist = range * measure.fraction + FEELER.maxs[0];

    // Top of the obstacle just past the face.
    let probe = mad(origin, dir, wall_dist + 6.0);
    let top_start = [
        probe[0],
        probe[1],
        origin[2] + t.vault_max_height + 2.0 - FEELER.mins[2],
    ];
    let top_end = [probe[0], probe[1], origin[2] + t.step_height];
    let down = trace(c, top_start, top_end, feeler(stand.mask));
    if down.startsolid != 0 || down.fraction >= 1.0 || down.normal[2] < 0.7 {
        return None;
    }
    let top_z = down.endpos[2] + FEELER.mins[2];
    let h = top_z - origin[2];
    if h > t.vault_max_height || wall_dist > reach.unwrap_or_else(|| t.vault_reach(h, speed)) {
        return None;
    }

    // Rise to clearance above the top.
    let clear_z = top_z + t.vault_clearance;
    let mid = [origin[0], origin[1], clear_z];
    let up = trace(c, origin, mid, stand);
    if up.startsolid != 0 || up.fraction < 1.0 {
        return None;
    }

    // Far edge: the first sample beyond the face with a real drop behind it.
    // Only samples in front of anything standing on the top count: on
    // triangle-soup collision (arena meshes) a sample inside a wall behind
    // the top is neither startsolid nor a hit, so it would read as a drop
    // and turn a step in front of a wall into a vault over it.
    let scan_z = top_z + 3.0 - FEELER.mins[2];
    let scan_from = mad(origin, dir, wall_dist + 6.0);
    let scan_from = [scan_from[0], scan_from[1], scan_z];
    let scan = trace(
        c,
        scan_from,
        mad(scan_from, dir, t.vault_long_depth + 8.0),
        feeler(stand.mask),
    );
    let open = if scan.startsolid != 0 {
        0.0
    } else {
        (t.vault_long_depth + 8.0) * scan.fraction
    };
    let mut depth = 0.0;
    let mut far_edge = None;
    while depth <= t.vault_long_depth && depth <= open {
        let p = mad(origin, dir, wall_dist + 6.0 + depth);
        let s = [p[0], p[1], top_z + 2.0];
        let e = [p[0], p[1], top_z - t.vault_onto_drop];
        let d = trace(c, s, e, feeler(stand.mask));
        if d.startsolid == 0 && d.fraction >= 1.0 {
            far_edge = Some(depth + 6.0);
            break;
        }
        depth += 8.0;
    }

    let (target, kind, depth) = match far_edge {
        Some(d) => {
            let land = mad(origin, dir, wall_dist + d + half_width + 2.0);
            let kind = if d <= t.vault_short_depth {
                script_kind::VAULT_OVER
            } else {
                script_kind::VAULT_OVER_LONG
            };
            ([land[0], land[1], clear_z], kind, d)
        }
        None => {
            let land = mad(origin, dir, wall_dist + half_width + 1.0);
            (
                [land[0], land[1], top_z + 0.25],
                script_kind::VAULT_ONTO,
                0.0,
            )
        }
    };
    if !fits(c, target, stand) {
        return None;
    }
    // The path: over the lip at the clearance height, then (vault onto) straight
    // down onto the top. A sweep from `mid` straight to a target only 0.25 above
    // the top grazes the lip edge, which clip-mesh traces report as a hit.
    let across = [target[0], target[1], clear_z];
    let over = trace(c, mid, across, stand);
    if over.fraction < 1.0 {
        return None;
    }
    if across[2] > target[2] {
        let down = trace(c, across, target, stand);
        if down.startsolid != 0 || down.fraction < 1.0 {
            return None;
        }
    }
    // Vault entry excludes a death height behind (PredictedDeathHeight ≥ 10 m).
    if kind != script_kind::VAULT_ONTO {
        let floor = [target[0], target[1], origin[2] - t.lethal_fall_height];
        let down = trace(c, target, floor, stand);
        if down.startsolid != 0 || down.fraction >= 1.0 || down.normal[2] < 0.7 {
            return None;
        }
    }
    Some(Vault {
        mid,
        target,
        onto: kind == script_kind::VAULT_ONTO,
        kind,
        top_z,
        wall_dist,
        depth,
    })
}

/// Raise `origin` by up to `lift` (coil release / uncoil on landing).
pub fn lift_clear<C: CollisionBackend>(c: &C, origin: V3, lift: f32, hull: Hull) -> V3 {
    let end = add(origin, [0.0, 0.0, lift]);
    let t = trace(c, origin, end, hull);
    if t.allsolid != 0 {
        origin
    } else {
        reached(&t, end)
    }
}
