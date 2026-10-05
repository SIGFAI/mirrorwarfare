//! Small `[f32; 3]` helpers. Kept local so the crate stays `no_std` and every
//! operation is a plain, ordered sequence of f32 ops (deterministic across
//! server, client prediction and replay).

pub type V3 = [f32; 3];

pub const ZERO: V3 = [0.0; 3];

#[inline]
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
pub fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
pub fn mad(a: V3, b: V3, s: f32) -> V3 {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

#[inline]
pub fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub fn len(a: V3) -> f32 {
    libm::sqrtf(dot(a, a))
}

#[inline]
pub fn horiz(a: V3) -> V3 {
    [a[0], a[1], 0.0]
}

#[inline]
pub fn hlen(a: V3) -> f32 {
    libm::sqrtf(a[0] * a[0] + a[1] * a[1])
}

/// Unit vector, or zero when the input is (near) zero.
#[inline]
pub fn norm(a: V3) -> V3 {
    let l = len(a);
    if l < 1.0e-6 { ZERO } else { scale(a, 1.0 / l) }
}

#[inline]
pub fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// IW4 `AngleVectors` with pitch = roll = 0: forward and right on the ground
/// plane for a yaw in degrees.
#[inline]
pub fn yaw_axes(yaw_deg: f32) -> (V3, V3) {
    let r = yaw_deg * (core::f32::consts::PI / 180.0);
    let (s, c) = (libm::sinf(r), libm::cosf(r));
    ([c, s, 0.0], [s, -c, 0.0])
}

/// Remove the component of `v` along plane normal `n` (with IW4's overbounce).
#[inline]
pub fn clip(v: V3, n: V3, overbounce: f32) -> V3 {
    let mut backoff = dot(v, n);
    if backoff < 0.0 {
        backoff *= overbounce;
    } else {
        backoff /= overbounce;
    }
    sub(v, scale(n, backoff))
}

/// Rotate horizontal vector `v` toward the horizontal direction `dir` by at most
/// `max_rad`, preserving its length. `dir` must be unit and horizontal.
pub fn rotate_toward(v: V3, dir: V3, max_rad: f32) -> V3 {
    let speed = hlen(v);
    if speed < 1.0e-3 {
        return v;
    }
    let cur = libm::atan2f(v[1], v[0]);
    let want = libm::atan2f(dir[1], dir[0]);
    let mut delta = want - cur;
    let pi = core::f32::consts::PI;
    while delta > pi {
        delta -= 2.0 * pi;
    }
    while delta < -pi {
        delta += 2.0 * pi;
    }
    let step = clampf(delta, -max_rad, max_rad);
    let a = cur + step;
    [libm::cosf(a) * speed, libm::sinf(a) * speed, v[2]]
}

#[inline]
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
