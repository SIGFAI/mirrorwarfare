//! Collision primitives over `movement_iw4::CollisionBackend`: hull traces,
//! ground check, Quake/IW4-style clip-plane slide and step-slide. Pure
//! functions of their inputs.

use movement_iw4::{CollisionBackend, GroundTraceInput};
use playerstate_iw4::ENTITYNUM_NONE;
use trace_iw4::{Trace, trace_get_entity_hit_id};

use crate::vec::{V3, add, clip, cross, dot, hlen, mad, norm, scale, sub};

const MAX_CLIP_PLANES: usize = 5;
const OVERBOUNCE: f32 = 1.001;
const GROUND_PROBE: f32 = 0.25;
/// Footprint radius (fraction of the half width) that still supports the body.
const SUPPORT_REACH: f32 = 0.7;
/// Wider footprint for a body already on the ground (`snap > 0`): hysteresis,
/// so walking along an edge right at the limit does not flip ground/air.
const SUPPORT_REACH_HELD: f32 = 0.85;
/// Support probe window around the resting hull bottom: the rounded bottom
/// sits up to ~0.3 half widths above a face under the footprint ring.
const SUPPORT_ABOVE: f32 = 5.0;
const SUPPORT_BELOW: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hull {
    pub mins: V3,
    pub maxs: V3,
    pub mask: u32,
}

impl Hull {
    #[must_use]
    pub fn with_maxs_z(self, z: f32) -> Self {
        Self {
            maxs: [self.maxs[0], self.maxs[1], z],
            ..self
        }
    }
}

#[inline]
pub fn trace<C: CollisionBackend>(c: &C, start: V3, end: V3, hull: Hull) -> Trace {
    c.trace(GroundTraceInput {
        start,
        end,
        mins: hull.mins,
        maxs: hull.maxs,
        tracemask: hull.mask,
    })
}

/// Point reached by a trace (`end` when unobstructed).
#[inline]
pub fn reached(t: &Trace, end: V3) -> V3 {
    if t.fraction >= 1.0 { end } else { t.endpos }
}

/// Is the hull free at `origin`?
pub fn fits<C: CollisionBackend>(c: &C, origin: V3, hull: Hull) -> bool {
    let t = trace(c, origin, origin, hull);
    t.startsolid == 0 && t.allsolid == 0
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ground {
    pub origin: V3,
    pub normal: V3,
    pub entity: i32,
}

/// Look for walkable ground under `origin`, up to `snap` below the probe.
pub fn ground<C: CollisionBackend>(
    c: &C,
    origin: V3,
    hull: Hull,
    snap: f32,
    min_walk_normal: f32,
) -> Option<Ground> {
    let start = [origin[0], origin[1], origin[2] + GROUND_PROBE];
    let end = [origin[0], origin[1], origin[2] - GROUND_PROBE - snap];
    let t = trace(c, start, end, hull);
    if t.allsolid != 0 || t.fraction >= 1.0 {
        return None;
    }
    let mut normal = t.normal;
    if normal[2] < min_walk_normal {
        // The rounded hull bottom on a step, seam or roof edge reports a
        // steep contact normal; the body is still supported while a walkable
        // face lies under its footprint, as IW4's capsule is until its centre
        // is ~0.7 radius past the edge.
        normal = support(c, t.endpos, hull, snap, min_walk_normal)?;
    }
    let entity = i32::from(trace_get_entity_hit_id(t.hit_type, t.hit_id));
    Some(Ground {
        origin: t.endpos,
        normal,
        entity: if entity == 0x7ff {
            ENTITYNUM_NONE
        } else {
            entity
        },
    })
}

/// Walkable face directly under the footprint of a hull resting at `at`:
/// thin probes at the centre, then a ring at `SUPPORT_REACH` of the half
/// width, reaching `snap` further down. Returns the first walkable normal.
pub fn support<C: CollisionBackend>(
    c: &C,
    at: V3,
    hull: Hull,
    snap: f32,
    min_walk_normal: f32,
) -> Option<V3> {
    let reach = if snap > 0.0 {
        SUPPORT_REACH_HELD
    } else {
        SUPPORT_REACH
    };
    let r = 0.5 * (hull.maxs[0] - hull.mins[0]) * reach;
    let cx = at[0] + 0.5 * (hull.maxs[0] + hull.mins[0]);
    let cy = at[1] + 0.5 * (hull.maxs[1] + hull.mins[1]);
    let z = at[2] + hull.mins[2];
    let thin = Hull {
        mins: [-0.5, -0.5, 0.0],
        maxs: [0.5, 0.5, 1.0],
        mask: hull.mask,
    };
    const D: f32 = core::f32::consts::FRAC_1_SQRT_2;
    let ring = [
        (0.0, 0.0),
        (1.0, 0.0),
        (-1.0, 0.0),
        (0.0, 1.0),
        (0.0, -1.0),
        (D, D),
        (-D, D),
        (D, -D),
        (-D, -D),
    ];
    for (dx, dy) in ring {
        let x = cx + dx * r;
        let y = cy + dy * r;
        let p = trace(
            c,
            [x, y, z + SUPPORT_ABOVE],
            [x, y, z - SUPPORT_BELOW - snap],
            thin,
        );
        if p.startsolid == 0 && p.fraction < 1.0 && p.normal[2] >= min_walk_normal {
            return Some(p.normal);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SlideOut {
    pub origin: V3,
    pub velocity: V3,
    pub blocked: bool,
    /// First steep (non-walkable) plane hit, if any.
    pub wall: Option<V3>,
}

/// PM_SlideMove: move `velocity * dt`, clipping against up to five planes.
pub fn slide_move<C: CollisionBackend>(
    c: &C,
    origin: V3,
    velocity: V3,
    dt: f32,
    hull: Hull,
    ground_normal: Option<V3>,
    min_walk_normal: f32,
) -> SlideOut {
    let mut o = origin;
    let mut v = velocity;
    let mut planes = [[0.0_f32; 3]; MAX_CLIP_PLANES];
    let mut n = 0usize;
    let mut wall = None;
    let mut blocked = false;
    if let Some(g) = ground_normal {
        planes[n] = g;
        n += 1;
    }
    planes[n] = norm(v);
    n += 1;

    let mut time_left = dt;
    for _ in 0..4 {
        if time_left <= 0.0 {
            break;
        }
        let end = mad(o, v, time_left);
        let t = trace(c, o, end, hull);
        if t.allsolid != 0 {
            v[2] = 0.0;
            return SlideOut {
                origin: o,
                velocity: v,
                blocked: true,
                wall,
            };
        }
        if t.fraction > 0.0 {
            o = reached(&t, end);
        }
        if t.fraction >= 1.0 {
            break;
        }
        blocked = true;
        if t.normal[2] < min_walk_normal && wall.is_none() {
            wall = Some(t.normal);
        }
        time_left -= time_left * t.fraction;
        if n >= MAX_CLIP_PLANES {
            v = [0.0; 3];
            break;
        }
        if planes.iter().take(n).any(|p| dot(t.normal, *p) > 0.99) {
            v = add(v, t.normal);
            continue;
        }
        planes[n] = t.normal;
        n += 1;

        let mut next = v;
        let mut stop = false;
        for i in 0..n {
            if dot(v, planes[i]) >= 0.1 {
                continue;
            }
            let mut cv = clip(v, planes[i], OVERBOUNCE);
            for j in 0..n {
                if j == i || dot(cv, planes[j]) >= 0.1 {
                    continue;
                }
                cv = clip(cv, planes[j], OVERBOUNCE);
                if dot(cv, planes[i]) >= 0.0 {
                    continue;
                }
                let dir = norm(cross(planes[i], planes[j]));
                cv = scale(dir, dot(dir, v));
                if planes
                    .iter()
                    .take(n)
                    .enumerate()
                    .any(|(k, p)| k != i && k != j && dot(cv, *p) < 0.1)
                {
                    stop = true;
                }
            }
            next = cv;
            break;
        }
        if stop {
            v = [0.0; 3];
            break;
        }
        v = next;
    }
    SlideOut {
        origin: o,
        velocity: v,
        blocked,
        wall,
    }
}

#[allow(clippy::too_many_arguments)]
/// PM_StepSlideMove: slide; if blocked on the ground, also try stepping up
/// `step` and keep whichever went further horizontally.
pub fn step_slide_move<C: CollisionBackend>(
    c: &C,
    origin: V3,
    velocity: V3,
    dt: f32,
    hull: Hull,
    ground_normal: Option<V3>,
    step: f32,
    min_walk_normal: f32,
) -> SlideOut {
    let flat = slide_move(
        c,
        origin,
        velocity,
        dt,
        hull,
        ground_normal,
        min_walk_normal,
    );
    if !flat.blocked || ground_normal.is_none() || step <= 0.0 {
        return flat;
    }
    let up_end = [origin[0], origin[1], origin[2] + step];
    let up = trace(c, origin, up_end, hull);
    if up.allsolid != 0 {
        return flat;
    }
    let raised = reached(&up, up_end);
    let lift = raised[2] - origin[2];
    if lift <= 0.0 {
        return flat;
    }
    let high = slide_move(c, raised, velocity, dt, hull, None, min_walk_normal);
    let down_end = [high.origin[0], high.origin[1], high.origin[2] - lift];
    let down = trace(c, high.origin, down_end, hull);
    let stepped = reached(&down, down_end);
    if down.fraction < 1.0
        && down.normal[2] < min_walk_normal
        && support(c, stepped, hull, 0.0, min_walk_normal).is_none()
    {
        return flat;
    }
    if hlen(sub(stepped, origin)) > hlen(sub(flat.origin, origin)) + 0.01 {
        SlideOut {
            origin: stepped,
            velocity: [
                high.velocity[0],
                high.velocity[1],
                flat.velocity[2].min(0.0),
            ],
            blocked: high.blocked,
            wall: high.wall,
        }
    } else {
        flat
    }
}
