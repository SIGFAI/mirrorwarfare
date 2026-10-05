//! Root-motion trajectories decoded from Faith's Raw-codec clips (DOF 969,
//! `context/artifacts/2026-10-04-mec-ant/evidence/rootmotion.txt`). Each key
//! is (t s, up m, forward m) relative to frame 0. Scripted moves sample these
//! *normalised* (time over the clip length, up over the peak rise, forward
//! over the total) and scale them to the geometry the probes found, so the
//! body follows the animation's timing on any obstacle size.

/// A clip trajectory: keys (t, up, forward).
#[derive(Clone, Copy, Debug)]
pub struct RootCurve {
    pub keys: &'static [(f32, f32, f32)],
}

/// #000856f3 VaultOverFast, 36 t (0.60 s), 3.70 m.
pub const VAULT_OVER_FAST: RootCurve = RootCurve {
    keys: &[
        (0.000, 0.000, 0.000),
        (0.067, 0.328, 0.394),
        (0.133, 0.614, 0.842),
        (0.200, 0.662, 1.222),
        (0.267, 0.662, 1.539),
        (0.333, 0.662, 1.869),
        (0.400, 0.552, 2.219),
        (0.467, 0.287, 2.631),
        (0.533, -0.041, 3.159),
        (0.600, -0.338, 3.700),
    ],
};

/// #00084bcd VaultOnto (Raw keys cover 0.5 s), up 0.96 m, forward 1.21 m.
pub const VAULT_ONTO: RootCurve = RootCurve {
    keys: &[
        (0.000, 0.000, 0.000),
        (0.067, 0.084, 0.374),
        (0.133, 0.288, 0.579),
        (0.200, 0.545, 0.679),
        (0.267, 0.784, 0.736),
        (0.333, 0.937, 0.812),
        (0.400, 0.959, 0.958),
        (0.467, 0.959, 1.124),
        (0.500, 0.959, 1.207),
    ],
};

/// #00087bf9 VaultOntoHigh, 74 t (1.233 s), up 1.25 m, forward 1.27 m.
pub const VAULT_ONTO_HIGH: RootCurve = RootCurve {
    keys: &[
        (0.000, 0.000, 0.000),
        (0.067, 0.035, 0.034),
        (0.133, 0.112, 0.115),
        (0.200, 0.189, 0.211),
        (0.267, 0.222, 0.298),
        (0.333, 0.231, 0.396),
        (0.400, 0.292, 0.494),
        (0.467, 0.433, 0.565),
        (0.533, 0.632, 0.625),
        (0.600, 0.849, 0.683),
        (0.667, 1.049, 0.739),
        (0.733, 1.195, 0.792),
        (0.800, 1.252, 0.844),
        (0.867, 1.252, 0.885),
        (0.933, 1.252, 0.920),
        (1.000, 1.252, 0.971),
        (1.067, 1.252, 1.064),
        (1.133, 1.252, 1.178),
        (1.200, 1.252, 1.260),
        (1.233, 1.252, 1.273),
    ],
};

/// #000855fa FallingLandRoll, 70 t (1.167 s), 4.40 m forward, no rise.
pub const LAND_ROLL: RootCurve = RootCurve {
    keys: &[
        (0.000, 0.0, 0.000),
        (0.067, 0.0, 0.415),
        (0.133, 0.0, 0.853),
        (0.200, 0.0, 1.268),
        (0.267, 0.0, 1.611),
        (0.333, 0.0, 1.848),
        (0.400, 0.0, 2.027),
        (0.467, 0.0, 2.171),
        (0.533, 0.0, 2.290),
        (0.600, 0.0, 2.396),
        (0.667, 0.0, 2.500),
        (0.733, 0.0, 2.614),
        (0.800, 0.0, 2.749),
        (0.867, 0.0, 3.016),
        (0.933, 0.0, 3.411),
        (1.000, 0.0, 3.792),
        (1.067, 0.0, 4.144),
        (1.133, 0.0, 4.367),
        (1.167, 0.0, 4.400),
    ],
};

impl RootCurve {
    /// Clip length (s).
    #[must_use]
    pub fn length(&self) -> f32 {
        self.keys[self.keys.len() - 1].0
    }

    /// Highest rise (m) — the normaliser for `up`.
    #[must_use]
    pub fn peak_up(&self) -> f32 {
        self.keys.iter().fold(0.0_f32, |m, k| m.max(k.1))
    }

    /// Total forward travel (m).
    #[must_use]
    pub fn total_forward(&self) -> f32 {
        self.keys[self.keys.len() - 1].2
    }

    /// Raw (up, forward) in metres at time `t` seconds (clamped).
    ///
    /// Monotone cubic (Fritsch–Carlson) through the keys: the position passes
    /// every key exactly and never overshoots between them (holds stay flat,
    /// peaks stay peaks), and — unlike straight lines between the 15 Hz keys
    /// — its velocity is continuous, so a body driven by it does not change
    /// speed in steps at every key.
    #[must_use]
    pub fn at(&self, t: f32) -> (f32, f32) {
        let k = self.keys;
        if t <= k[0].0 {
            return (k[0].1, k[0].2);
        }
        for i in 0..k.len() - 1 {
            let (a, b) = (k[i], k[i + 1]);
            if t <= b.0 {
                let h = b.0 - a.0;
                let f = (t - a.0) / h;
                let up = hermite(a.1, b.1, self.slope(i, 1), self.slope(i + 1, 1), h, f);
                let fwd = hermite(a.2, b.2, self.slope(i, 2), self.slope(i + 1, 2), h, f);
                return (up, fwd);
            }
        }
        let l = k[k.len() - 1];
        (l.1, l.2)
    }

    /// Fritsch–Carlson tangent at key `i` of channel `c` (1 = up, 2 = forward).
    fn slope(&self, i: usize, c: usize) -> f32 {
        let k = self.keys;
        let v = |j: usize| if c == 1 { k[j].1 } else { k[j].2 };
        let d = |j: usize| (v(j + 1) - v(j)) / (k[j + 1].0 - k[j].0);
        let n = k.len();
        if n < 2 {
            return 0.0;
        }
        if i == 0 {
            return d(0);
        }
        if i == n - 1 {
            return d(n - 2);
        }
        let (d0, d1) = (d(i - 1), d(i));
        if d0 * d1 <= 0.0 {
            return 0.0;
        }
        // Harmonic mean, weighted by the neighbouring intervals.
        let (h0, h1) = (k[i].0 - k[i - 1].0, k[i + 1].0 - k[i].0);
        let (w0, w1) = (2.0 * h1 + h0, h1 + 2.0 * h0);
        (w0 + w1) / (w0 / d0 + w1 / d1)
    }

    /// Normalised (up / peak, forward / total) at clip fraction `f` (0..=1).
    #[must_use]
    pub fn normalized(&self, f: f32) -> (f32, f32) {
        let (up, fwd) = self.at(f.clamp(0.0, 1.0) * self.length());
        let peak = self.peak_up();
        let total = self.total_forward();
        (
            if peak > 1.0e-6 { up / peak } else { 0.0 },
            if total > 1.0e-6 { fwd / total } else { 0.0 },
        )
    }

    /// Clip fraction at which the rise first reaches its peak.
    #[must_use]
    pub fn peak_fraction(&self) -> f32 {
        let peak = self.peak_up();
        self.keys
            .iter()
            .find(|k| k.1 >= peak - 1.0e-4)
            .map_or(1.0, |k| k.0 / self.length())
    }

    /// Forward speed (m/s) at time `t`, from the neighbouring keys.
    #[must_use]
    pub fn forward_speed(&self, t: f32) -> f32 {
        let h = 1.0 / 30.0;
        let a = self.at((t - h).max(0.0)).1;
        let b = self.at((t + h).min(self.length())).1;
        let span = (t + h).min(self.length()) - (t - h).max(0.0);
        if span > 0.0 { (b - a) / span } else { 0.0 }
    }
}

/// Cubic Hermite between `p0` and `p1` over an interval of `h` s with end
/// tangents `m0` / `m1` (per s), at fraction `f`.
fn hermite(p0: f32, p1: f32, m0: f32, m1: f32, h: f32, f: f32) -> f32 {
    let f2 = f * f;
    let f3 = f2 * f;
    (2.0 * f3 - 3.0 * f2 + 1.0) * p0
        + (f3 - 2.0 * f2 + f) * h * m0
        + (-2.0 * f3 + 3.0 * f2) * p1
        + (f3 - f2) * h * m1
}
