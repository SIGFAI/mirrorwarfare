//! Behaviour tests over a synthetic axis-aligned-box world.

extern crate std;

use std::vec::Vec;

use movement_iw4::{ANGLE2SHORT, CollisionBackend, GroundTraceInput};
use playerstate_iw4::{ENTITYNUM_NONE, PlayerState, UserCmd, buttons};
use trace_iw4::{ENTITYNUM_WORLD, HITTYPE_ENTITY, Trace};

use crate::rootmotion::{LAND_ROLL, VAULT_ONTO_HIGH};
use crate::{
    METRE, MecContext, MecEvent, MecMode, MecMoveResult, MecMoveState, MecTuning, MecUnlocks, TICK,
    air_flags, mec_pmove, script_kind,
};

const EPS: f32 = 0.125;
const TOUCH: f32 = 1.0e-4;
const MSEC: i32 = 33;

/// Boxes as (mins, maxs). Swept-AABB traces (Minkowski + slab test).
struct BoxWorld {
    boxes: Vec<([f32; 3], [f32; 3])>,
}

impl BoxWorld {
    fn floor() -> Self {
        Self {
            boxes: std::vec![([-20000.0, -20000.0, -64.0], [20000.0, 20000.0, 0.0])],
        }
    }
    fn with(mut self, mins: [f32; 3], maxs: [f32; 3]) -> Self {
        self.boxes.push((mins, maxs));
        self
    }
}

impl CollisionBackend for BoxWorld {
    fn trace(&self, i: GroundTraceInput) -> Trace {
        let d = [
            i.end[0] - i.start[0],
            i.end[1] - i.start[1],
            i.end[2] - i.start[2],
        ];
        let dlen = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let mut best = 1.0_f32;
        let mut best_n = [0.0; 3];
        let mut startsolid = false;
        let mut allsolid = false;
        for (bmin, bmax) in &self.boxes {
            let emin = [
                bmin[0] - i.maxs[0],
                bmin[1] - i.maxs[1],
                bmin[2] - i.maxs[2],
            ];
            let emax = [
                bmax[0] - i.mins[0],
                bmax[1] - i.mins[1],
                bmax[2] - i.mins[2],
            ];
            let inside =
                |p: [f32; 3]| (0..3).all(|a| p[a] > emin[a] + TOUCH && p[a] < emax[a] - TOUCH);
            if inside(i.start) {
                startsolid = true;
                if inside(i.end) {
                    allsolid = true;
                }
                continue;
            }
            let mut tin = f32::NEG_INFINITY;
            let mut tout = f32::INFINITY;
            let mut n = [0.0; 3];
            let mut miss = false;
            for a in 0..3 {
                if d[a].abs() < 1.0e-9 {
                    if i.start[a] <= emin[a] + TOUCH || i.start[a] >= emax[a] - TOUCH {
                        miss = true;
                    }
                    continue;
                }
                let t1 = (emin[a] - i.start[a]) / d[a];
                let t2 = (emax[a] - i.start[a]) / d[a];
                let (near, far, sign) = if d[a] > 0.0 {
                    (t1, t2, -1.0)
                } else {
                    (t2, t1, 1.0)
                };
                if near > tin {
                    tin = near;
                    n = [0.0; 3];
                    n[a] = sign;
                }
                if far < tout {
                    tout = far;
                }
            }
            if miss || tin >= tout || tin < -TOUCH || tin > 1.0 {
                continue;
            }
            let f = (tin - EPS / dlen.max(1.0e-6)).max(0.0);
            if f < best {
                best = f;
                best_n = n;
            }
        }
        let fraction = if allsolid { 0.0 } else { best };
        Trace {
            fraction,
            normal: best_n,
            startsolid: u8::from(startsolid),
            allsolid: u8::from(allsolid),
            hit_type: HITTYPE_ENTITY,
            hit_id: ENTITYNUM_WORLD,
            endpos: [
                i.start[0] + d[0] * fraction,
                i.start[1] + d[1] * fraction,
                i.start[2] + d[2] * fraction,
            ],
            ..Trace::default()
        }
    }
}

struct Sim {
    ps: PlayerState,
    mec: MecMoveState,
    time: i32,
    yaw: f32,
    tuning: MecTuning,
    log: Vec<(PlayerState, MecMoveState, MecMoveResult)>,
}

impl Sim {
    fn new(origin: [f32; 3], velocity: [f32; 3], yaw: f32) -> Self {
        let mut ps = PlayerState::ZERO;
        ps.origin = origin;
        ps.velocity = velocity;
        ps.ground_entity_num = ENTITYNUM_NONE;
        let mut mec = MecMoveState::SPAWN;
        mec.move_from = origin;
        mec.fall_start_z = origin[2];
        Self {
            ps,
            mec,
            time: 0,
            yaw,
            tuning: base_tuning(),
            log: Vec::new(),
        }
    }

    /// Start airborne at `origin` (jump start there).
    fn in_air(origin: [f32; 3], velocity: [f32; 3], yaw: f32) -> Self {
        let mut s = Self::new(origin, velocity, yaw);
        s.mec.mode = MecMode::Air;
        s
    }

    fn step<W: CollisionBackend>(&mut self, world: &W, forward: i8, held: u32) -> MecMoveResult {
        self.time += MSEC;
        let mut cmd = UserCmd {
            server_time: self.time,
            buttons: held,
            forwardmove: forward,
            ..UserCmd::default()
        };
        cmd.angles[1] = (self.yaw * ANGLE2SHORT) as i32 & 0xffff;
        let mut ctx = MecContext::new(MSEC);
        ctx.tuning = self.tuning;
        let res = mec_pmove(&mut self.ps, &mut self.mec, &cmd, &ctx, world);
        self.log.push((self.ps, self.mec, res));
        res
    }

    fn run<W: CollisionBackend>(&mut self, world: &W, ticks: usize, forward: i8, held: u32) {
        for _ in 0..ticks {
            self.step(world, forward, held);
        }
    }

    fn hspeed(&self) -> f32 {
        (self.ps.velocity[0].powi(2) + self.ps.velocity[1].powi(2)).sqrt()
    }

    fn events(&self) -> Vec<MecEvent> {
        self.log
            .iter()
            .flat_map(|(_, _, r)| r.iter_events())
            .collect()
    }

    fn ticks_in(&self, mode: MecMode) -> usize {
        self.log.iter().filter(|(_, m, _)| m.mode == mode).count()
    }

    fn secs_in(&self, mode: MecMode) -> f32 {
        self.ticks_in(mode) as f32 * DT
    }

    fn first(&self, mode: MecMode) -> Option<usize> {
        self.log.iter().position(|(_, m, _)| m.mode == mode)
    }

    fn last(&self, mode: MecMode) -> Option<usize> {
        self.log.iter().rposition(|(_, m, _)| m.mode == mode)
    }
}

const DT: f32 = MSEC as f32 / 1000.0;

/// The decoded base values (nothing unlocked).
fn t() -> MecTuning {
    MecTuning::BASE
}

/// What the behaviour tests run on: the base counts (1 wallrun / 2 wall moves
/// per airtime, 3 s slide) with coil and quickturn on, as before unlock gating.
fn base_tuning() -> MecTuning {
    MecTuning::BASE.with_unlocks(MecUnlocks {
        coil: true,
        quickturn: true,
        ..MecUnlocks::NONE
    })
}

fn m(x: f32) -> f32 {
    x * METRE
}

/// `|a - b| <= tol` with a readable failure.
#[track_caller]
fn near(what: &str, a: f32, b: f32, tol: f32) {
    assert!((a - b).abs() <= tol, "{what}: {a} vs {b} (±{tol})");
}

// ------------------------------------------------------------------ tuning

#[test]
fn tuning_carries_the_decoded_numbers() {
    let t = t();
    near("gravity", t.gravity, 773.2, 0.1);
    near("step", t.step_height, 9.45, 0.01);
    near("walk", t.walk_speed, m(2.0), 1e-3);
    near("crouch", t.crouch_speed, m(2.0), 1e-3);
    near("run start", t.run_start_speed(), m(2.0), m(0.01));
    near("run 0.25 s", t.run_speed_at(0.25), m(4.0), m(0.05));
    near("run 1.0 s", t.run_speed_at(1.0), m(6.7), m(0.05));
    near("run 3.0 s", t.run_speed_at(3.0), m(7.2), m(0.01));
    let rows = [
        (0.0, 0.0, 1.1),
        (0.5, 2.28, 1.1),
        (2.0, 4.12, 1.1),
        (3.6, 5.96, 1.1),
        (6.5, 8.04, 1.2),
    ];
    for (r, (lo, fwd, h)) in crate::JUMP_TABLE.iter().zip(rows) {
        near("jump row speed", r.min_speed, m(lo), 1e-3);
        near("jump row forward", r.forward_speed, m(fwd), 1e-3);
        near("jump row height", r.height, m(h), 1e-3);
    }
    near("coil", t.coil_lift, m(0.64), 1e-3);
    near("wallrun time", t.wallrun_time, 80.0 / 60.0, 1e-4);
    near("wallrun 2nd", t.wallrun_second_time, 64.0 / 60.0, 1e-4);
    near("wallrun apex", t.wallrun_apex_time, 32.0 / 60.0, 1e-4);
    near("wallrun height", t.wallrun_height, m(1.2), 1e-3);
    near("wallrun fall gate", t.wallrun_max_fall_speed, m(3.0), 1e-3);
    near("wallrun jump gate", t.wallrun_max_jump_dist, m(8.0), 1e-3);
    near(
        "wallrun jump gate falling",
        t.wallrun_max_jump_dist_falling,
        m(4.0),
        1e-3,
    );
    assert_eq!((t.wallruns_per_air, t.wall_moves_per_air), (1, 2));
    near("new wall", t.new_wall_min_angle_deg, 45.0, 0.0);
    near("wallclimb height", t.wallclimb_height, m(2.6), 1e-3);
    near("wallclimb apex", t.wallclimb_apex_time, 1.0, 1e-4);
    near(
        "wallclimb fall gate",
        t.wallclimb_max_fall_speed,
        m(5.0),
        1e-3,
    );
    near("wallclimb dist", t.wallclimb_max_dist, m(0.8), 1e-3);
    near("wallclimb angle", t.wallclimb_max_angle_deg, 40.0, 0.0);
    near("ledge cap", t.ledge_max_above_jump, m(4.49), 1e-3);
    near(
        "vault reach h1 v0",
        t.vault_reach(m(1.0), 0.0),
        m(1.0),
        1e-3,
    );
    near(
        "vault reach h1 v7.2",
        t.vault_reach(m(1.0), m(7.2)),
        m(1.6),
        1e-3,
    );
    near("vault height", t.vault_max_height, m(1.7), 1e-3);
    near("vault short depth", t.vault_short_depth, m(1.4), 1e-3);
    near("vault long depth", t.vault_long_depth, m(4.0), 1e-3);
    near("vault short time", t.vault_short_time, 0.6, 1e-4);
    near("vault long time", t.vault_long_time, 0.75, 1e-4);
    near("vault exit min", t.vault_exit_min_speed, m(4.0), 1e-3);
    near("vault exit max", t.vault_exit_max_speed, m(6.5), 1e-3);
    near("springboard min", t.springboard_obstacle_min, m(0.9), 1e-3);
    near("springboard max", t.springboard_obstacle_max, m(1.5), 1e-3);
    near("slide abort", t.slide_min_speed, m(4.0), 1e-3);
    near("slide max", t.slide_max_time, 3.0, 0.0);
    near("quickturn", t.quickturn_time, 36.0 / 60.0, 1e-4);
    near(
        "quickturn move-out",
        t.quickturn_move_out,
        32.0 / 60.0,
        1e-4,
    );
    near("stumble height", t.stumble_height, m(2.0), 1e-3);
    near("stumble time", t.stumble_time, 0.633, 1e-3);
    near("fail medium", t.fail_medium_height, m(4.0), 1e-3);
    near("fail medium time", t.fail_medium_time, 2.2, 1e-3);
    near("fail", t.fail_height, m(6.0), 1e-3);
    near("fail time", t.fail_time, 3.367, 1e-3);
    near("death", t.lethal_fall_height, m(10.0), 1e-3);
    near("roll drop", t.roll_min_drop, m(1.0), 1e-3);
    near("roll time", t.roll_time, 1.167, 1e-3);
    near("roll move-out", t.roll_move_out, 1.0, 1e-4);
    near("roll dist", t.roll_dist, m(4.4), 1e-3);
    near("roll curve", LAND_ROLL.total_forward(), 4.4, 1e-3);
    near("roll curve len", LAND_ROLL.length(), t.roll_time, 1e-3);
    near("wallrun camera roll", t.wallrun_camera_roll_deg, 6.0, 0.0);
    assert!(t.fail_damage.1 < 100.0, "non-lethal below 10 m");
}

// --------------------------------------------------------------- ground

#[test]
fn sprint_follows_the_decoded_speed_curve() {
    let world = BoxWorld::floor();
    let mut s = Sim::new([0.0, 0.0, 0.125], [m(2.0), 0.0, 0.0], 0.0);
    let at = |k: usize, s: &mut Sim| {
        while s.log.len() < k {
            s.step(&world, 127, 0);
        }
        s.hspeed()
    };
    let v8 = at(8, &mut s); // 0.264 s
    near("v(0.264 s)", v8, t().run_speed_at(8.0 * DT), 2.0);
    near("~4.0 m/s at 0.25 s", v8, m(4.0), m(0.15));
    let v30 = at(30, &mut s); // 0.99 s
    near("~6.7 m/s at 1.0 s", v30, m(6.7), m(0.1));
    let v91 = at(91, &mut s); // 3.0 s
    near("7.2 m/s at 3 s", v91, m(7.2), m(0.02));
    assert!(s.mec.momentum > 0.74);
    assert_eq!(s.mec.mode, MecMode::Ground);

    // Full speed into a tall wall: stops dead, momentum gone.
    let x = s.ps.origin[0];
    let world = world.with([x + 150.0, -2000.0, 0.0], [x + 250.0, 2000.0, 600.0]);
    s.run(&world, 30, 127, 0);
    assert!(s.hspeed() < 10.0, "blocked: {}", s.hspeed());
    assert!(s.mec.momentum < 0.05, "momentum lost: {}", s.mec.momentum);
}

#[test]
fn walk_and_crouch_are_two_metres_per_second() {
    let world = BoxWorld::floor();
    let mut s = Sim::new([0.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.run(&world, 30, -127, 0);
    near("walk back", s.hspeed(), m(2.0), 0.5);
    let mut s = Sim::new([0.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.run(&world, 30, 127, buttons::CROUCH);
    near("crouch", s.hspeed(), m(2.0), 0.5);
}

#[test]
fn jumps_follow_the_jump_database() {
    for (entry, fwd, height) in [
        (0.2, 0.0, 1.1),
        (1.0, 2.28, 1.1),
        (3.0, 4.12, 1.1),
        (5.0, 5.96, 1.1),
        (7.0, 8.04, 1.2),
    ] {
        let world = BoxWorld::floor();
        let mut s = Sim::new([0.0, 0.0, 0.125], [m(entry), 0.0, 0.0], 0.0);
        s.mec.momentum = 1.0;
        s.step(&world, 0, buttons::JUMP);
        assert!(s.events().contains(&MecEvent::Jump), "{entry}");
        near("jump forward speed", s.hspeed(), m(fwd), 0.5);
        let mut peak = 0.0_f32;
        for _ in 0..60 {
            s.step(&world, 0, 0);
            peak = peak.max(s.ps.origin[2]);
        }
        near("jump apex", peak, m(height) + 0.125, 1.5);
    }
}

#[test]
fn quickturn_turns_by_its_move_out() {
    let world = BoxWorld::floor();
    let mut s = Sim::new([0.0, 0.0, 0.125], [0.0; 3], 30.0);
    s.step(&world, 0, 0);
    let before = s.ps.viewangles[1];
    let turned = |s: &Sim| {
        let mut d = s.ps.viewangles[1] - before;
        while d < 0.0 {
            d += 360.0;
        }
        d
    };
    s.step(&world, 0, MecContext::QUICKTURN_BUTTON_DEFAULT);
    s.run(&world, 13, 0, 0); // 14 ticks = 0.462 s
    assert!(
        turned(&s) < 178.0,
        "still turning at 0.46 s: {}",
        turned(&s)
    );
    s.run(&world, 3, 0, 0); // 17 ticks = 0.561 s > 32 t
    near("turned", turned(&s), 180.0, 0.5);
    assert!(s.events().contains(&MecEvent::QuickTurn));
}

#[test]
fn slide_aborts_below_four_metres_per_second() {
    let world = BoxWorld::floor();
    let v = t().run_max_speed;
    let mut s = Sim::new([0.0, 0.0, 0.125], [v, 0.0, 0.0], 0.0);
    s.mec.momentum = 1.0;
    s.step(&world, 127, 0);
    let r = s.step(&world, 127, buttons::CROUCH);
    assert!(r.iter_events().any(|e| e == MecEvent::SlideStart));
    assert!((r.maxs[2] - 50.0).abs() < 1e-3 && (r.view_height - 40.0).abs() < 1e-3);
    s.run(&world, 60, 127, buttons::CROUCH);
    let last = s.last(MecMode::Slide).expect("slid");
    let end_speed = hspeed_at(&s, last + 1);
    assert!(
        end_speed < m(4.0) && end_speed > m(3.6),
        "abort speed {end_speed}"
    );
    // Slide curve from 7.2 m/s to 4 m/s: 1.02 s.
    near("slide time 7.2 → 4", s.secs_in(MecMode::Slide), 1.02, 0.07);
}

#[test]
fn slide_lasts_at_most_three_seconds() {
    let world = BoxWorld::floor();
    let mut s = Sim::new([0.0, 0.0, 0.125], [m(10.0), 0.0, 0.0], 0.0);
    s.tuning.slide_min_speed = 0.0;
    s.mec.momentum = 1.0;
    s.step(&world, 0, buttons::CROUCH);
    s.run(&world, 120, 0, buttons::CROUCH);
    near("slide max time", s.secs_in(MecMode::Slide), 3.0, 0.07);
}

// -------------------------------------------------------------- wallrun

/// Wall face at y = 40 (left of +x travel), 10 m high, 80 m long; runner in
/// the air at z 100 moving along it at a slight angle.
fn wallrun_sim(speed: f32, vz: f32) -> (BoxWorld, Sim) {
    let world = BoxWorld::floor().with([-200.0, 40.0, 0.0], [3200.0, 120.0, 400.0]);
    let r = 10.0_f32.to_radians();
    let s = Sim::in_air(
        [0.0, 10.0, 100.0],
        [speed * r.cos(), speed * r.sin(), vz],
        10.0,
    );
    (world, s)
}

#[test]
fn wallrun_is_a_scripted_arc_of_the_decoded_length() {
    let (world, mut s) = wallrun_sim(t().run_max_speed, 0.0);
    s.step(&world, 127, 0);
    s.run(&world, 70, 127, buttons::JUMP);
    let first = s.first(MecMode::WallRun).expect("wallrun engaged");
    assert!(
        s.events()
            .iter()
            .any(|e| matches!(e, MecEvent::WallRunStart { side: -1 }))
    );
    near("wallrun time", s.secs_in(MecMode::WallRun), 80.0 * TICK, DT);
    let z0 = s.log[first - 1].0.origin[2];
    let (peak_i, peak) = s
        .log
        .iter()
        .enumerate()
        .map(|(i, (p, _, _))| (i, p.origin[2]))
        .fold((0, f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
    near("wallrun rise", peak - z0, m(1.2), 1.5);
    near(
        "wallrun apex time",
        (peak_i - first + 1) as f32 * DT,
        32.0 * TICK,
        DT * 1.5,
    );
    let last = s.last(MecMode::WallRun).unwrap();
    let along = s.log[last].0.origin[0] - s.log[first - 1].0.origin[0];
    near(
        "7.2 m/s along the wall",
        along / s.secs_in(MecMode::WallRun),
        m(7.2),
        8.0,
    );
    // One wallrun per airtime.
    let starts = s
        .events()
        .iter()
        .filter(|e| matches!(e, MecEvent::WallRunStart { .. }))
        .count();
    assert_eq!(starts, 1);
}

#[test]
fn wallrun_has_no_minimum_speed() {
    let (world, mut s) = wallrun_sim(m(2.0), 0.0);
    s.run(&world, 10, 127, buttons::JUMP);
    let i = s.first(MecMode::WallRun).expect("slow wallrun engages");
    // Length clamp 4 m over the 1.333 s wallrun: at least 3 m/s along it.
    near(
        "min along speed",
        hspeed_at(&s, i),
        m(4.0) / (80.0 * TICK),
        1.0,
    );
}

#[test]
fn wallrun_gates() {
    // Falling faster than 3 m/s.
    let (world, mut s) = wallrun_sim(t().run_max_speed, -m(3.2));
    s.run(&world, 6, 127, buttons::JUMP);
    assert!(s.first(MecMode::WallRun).is_none(), "fall speed gate");
    // Jump start 9 m back.
    let (world, mut s) = wallrun_sim(t().run_max_speed, 0.0);
    s.mec.move_from = [-m(9.0), 10.0, 100.0];
    s.run(&world, 6, 127, buttons::JUMP);
    assert!(
        s.first(MecMode::WallRun).is_none(),
        "8 m jump distance gate"
    );
    // 5 m back is fine while rising, not once falling (4 m).
    let (world, mut s) = wallrun_sim(t().run_max_speed, 60.0);
    s.mec.move_from = [-m(5.0), 10.0, 100.0];
    s.run(&world, 3, 127, buttons::JUMP);
    assert!(s.first(MecMode::WallRun).is_some(), "5 m while rising");
    let (world, mut s) = wallrun_sim(t().run_max_speed, -m(1.0));
    s.mec.move_from = [-m(5.0), 10.0, 100.0];
    s.run(&world, 6, 127, buttons::JUMP);
    assert!(s.first(MecMode::WallRun).is_none(), "4 m once falling");
    // Already wallran this airtime (another wall).
    let (world, mut s) = wallrun_sim(t().run_max_speed, 0.0);
    s.mec.air_flags = air_flags::add_wall_move(air_flags::USED_WALLRUN);
    s.mec.last_wall_normal = [1.0, 0.0, 0.0];
    s.run(&world, 6, 127, buttons::JUMP);
    assert!(s.first(MecMode::WallRun).is_none(), "1 wallrun per airtime");
    // Same wall again (< 45°) is never a new wall.
    let (world, mut s) = wallrun_sim(t().run_max_speed, 0.0);
    s.tuning.wallruns_per_air = 2;
    s.mec.air_flags = air_flags::add_wall_move(air_flags::USED_WALLRUN);
    s.mec.last_wall_normal = [0.2, -0.98, 0.0];
    s.run(&world, 6, 127, buttons::JUMP);
    assert!(s.first(MecMode::WallRun).is_none(), "same wall");
    // Two wall moves used.
    let (world, mut s) = wallrun_sim(t().run_max_speed, 0.0);
    s.tuning.wallruns_per_air = 2;
    s.mec.air_flags = air_flags::add_wall_move(air_flags::add_wall_move(0));
    s.run(&world, 6, 127, buttons::JUMP);
    assert!(
        s.first(MecMode::WallRun).is_none(),
        "2 wall moves per airtime"
    );
}

#[test]
fn second_wallrun_is_shorter() {
    let (world, mut s) = wallrun_sim(t().run_max_speed, 0.0);
    s.tuning.wallruns_per_air = 2;
    s.mec.air_flags = air_flags::add_wall_move(air_flags::USED_WALLRUN);
    s.mec.last_wall_normal = [1.0, 0.0, 0.0];
    s.run(&world, 60, 127, buttons::JUMP);
    near(
        "second wallrun",
        s.secs_in(MecMode::WallRun),
        64.0 * TICK,
        DT,
    );
}

#[test]
fn wall_jump_pushes_away_from_wall() {
    let (world, mut s) = wallrun_sim(t().run_max_speed, 0.0);
    while s.mec.mode != MecMode::WallRun && s.log.len() < 30 {
        s.step(&world, 127, buttons::JUMP);
    }
    assert_eq!(s.mec.mode, MecMode::WallRun);
    s.run(&world, 5, 127, buttons::JUMP);
    s.step(&world, 127, 0);
    s.step(&world, 127, buttons::JUMP); // fresh press = wall jump
    assert!(s.events().contains(&MecEvent::WallJump));
    assert!(
        s.ps.velocity[1] < -50.0,
        "pushed off (-y): {:?}",
        s.ps.velocity
    );
    assert!(s.ps.velocity[2] > 0.0);
}

// ------------------------------------------------------------ wallclimb

#[test]
fn wallclimb_rises_two_point_six_metres_in_one_second() {
    let world = BoxWorld::floor().with([100.0, -500.0, 0.0], [300.0, 500.0, 1000.0]);
    let mut s = Sim::new([75.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 127, 0);
    s.run(&world, 90, 127, buttons::JUMP);
    assert!(s.events().contains(&MecEvent::WallClimbStart));
    assert!(s.events().contains(&MecEvent::WallClimbEnd));
    let first = s.first(MecMode::WallClimb).unwrap();
    let (peak_i, peak) = s
        .log
        .iter()
        .enumerate()
        .map(|(i, (p, _, _))| (i, p.origin[2]))
        .fold((0, f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
    near("wallclimb rise", peak, m(2.6), 1.5);
    near(
        "wallclimb apex time",
        (peak_i - first + 1) as f32 * DT,
        1.0,
        DT * 1.5,
    );
    near(
        "on the wall",
        s.secs_in(MecMode::WallClimb),
        100.0 * TICK,
        DT * 1.5,
    );
    assert_eq!(
        s.events()
            .iter()
            .filter(|e| **e == MecEvent::WallClimbStart)
            .count(),
        1
    );
    assert_eq!(s.mec.mode, MecMode::Ground);
    assert!(s.ps.origin[2] < 1.0);
}

#[test]
fn wallclimb_gates() {
    let world = BoxWorld::floor().with([100.0, -500.0, 0.0], [300.0, 500.0, 1000.0]);
    // Wall 0.9 m from the body axis: too far.
    let mut s = Sim::new([100.0 - m(0.9), 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 0, 0);
    s.run(&world, 20, 0, buttons::JUMP);
    assert!(
        !s.events().contains(&MecEvent::WallClimbStart),
        "0.9 m away"
    );
    // 0.75 m is within the 0.8 m gate.
    let mut s = Sim::new([100.0 - m(0.75), 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 0, 0);
    s.step(&world, 0, buttons::JUMP);
    assert!(
        s.events().contains(&MecEvent::WallClimbStart),
        "0.75 m away"
    );
    // Falling faster than 5 m/s.
    let mut s = Sim::in_air([75.0, 0.0, 300.0], [0.0, 0.0, -m(5.5)], 0.0);
    s.run(&world, 3, 0, buttons::JUMP);
    assert!(!s.events().contains(&MecEvent::WallClimbStart), "fall gate");
    // 40° off the wall is fine, 50° is not.
    for (yaw, ok) in [(38.0, true), (50.0, false)] {
        let mut s = Sim::new([80.0, 0.0, 0.125], [0.0; 3], yaw);
        s.step(&world, 0, 0);
        s.step(&world, 0, buttons::JUMP);
        assert_eq!(s.events().contains(&MecEvent::WallClimbStart), ok, "{yaw}°");
    }
}

#[test]
fn ledge_climb_from_wallclimb_follows_the_root_curve() {
    // 150 in (3.8 m) wall: too high to grab from the ground, reachable via wallclimb.
    let world = BoxWorld::floor().with([100.0, -500.0, 0.0], [400.0, 500.0, 150.0]);
    let mut s = Sim::new([75.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 127, 0);
    s.run(&world, 60, 127, buttons::JUMP);
    let ev = s.events();
    assert!(ev.contains(&MecEvent::WallClimbStart));
    assert!(ev.contains(&MecEvent::LedgeClimbStart));
    assert!(
        s.ps.origin[2] > 149.0 && s.ps.origin[2] < 152.0,
        "on top: {:?}",
        s.ps.origin
    );
    assert!(s.ps.origin[0] > 100.0, "over the lip: {:?}", s.ps.origin);
    assert_eq!(s.mec.mode, MecMode::Ground);
    // HangHeaveUp move-out: 1.0 s.
    near(
        "ledge climb time",
        s.secs_in(MecMode::LedgeClimb),
        1.0,
        DT * 1.5,
    );
    let first = s.first(MecMode::LedgeClimb).unwrap();
    let (_, m0, _) = s.log[first];
    assert_eq!(m0.wall_side, script_kind::LEDGE_HIGH);
    let z_from = m0.move_from[2];
    let z_to = m0.move_to[2];
    // Height follows VaultOntoHigh: normalised rise at each tick.
    for (p, mm, _) in s
        .log
        .iter()
        .filter(|(_, mm, _)| mm.mode == MecMode::LedgeClimb)
    {
        let f = mm.mode_ms as f32 / mm.move_ms as f32;
        let (up, _) = VAULT_ONTO_HIGH.normalized(f);
        // The hang is softened (40% smoothstep to the peak) so it never stalls.
        let x = (f / VAULT_ONTO_HIGH.peak_fraction()).clamp(0.0, 1.0);
        let up = 0.6 * up + 0.4 * x * x * (3.0 - 2.0 * x);
        near(
            "rise follows the curve",
            (p.origin[2] - z_from) / (z_to - z_from),
            up,
            0.04,
        );
    }
    assert!(s.log.iter().any(|(_, mm, r)| mm.mode == MecMode::LedgeClimb
        && r.gates.lowered
        && !r.gates.allow_fire));
}

#[test]
fn ledge_above_four_point_five_metres_is_out_of_reach() {
    // 4.6 m wall: wallclimb peak (2.6 m) + arm reach would get there, but the
    // top is more than 4.49 m above the jump start.
    let world = BoxWorld::floor().with([100.0, -500.0, 0.0], [400.0, 500.0, m(4.6)]);
    let mut s = Sim::new([75.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 127, 0);
    s.run(&world, 90, 127, buttons::JUMP);
    assert!(s.events().contains(&MecEvent::WallClimbStart));
    assert!(!s.events().contains(&MecEvent::LedgeClimbStart));
    // 4.3 m is reachable.
    let world = BoxWorld::floor().with([100.0, -500.0, 0.0], [400.0, 500.0, m(4.3)]);
    let mut s = Sim::new([75.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 127, 0);
    s.run(&world, 90, 127, buttons::JUMP);
    assert!(s.events().contains(&MecEvent::LedgeClimbStart));
}

#[test]
fn chest_high_ledge_is_climbed_from_standing_jump() {
    let world = BoxWorld::floor().with([100.0, -500.0, 0.0], [400.0, 500.0, 60.0]);
    let mut s = Sim::new([80.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 0, 0);
    s.step(&world, 0, buttons::JUMP);
    assert!(s.events().contains(&MecEvent::LedgeClimbStart));
    assert_eq!(s.mec.wall_side, script_kind::LEDGE_LOW);
    s.run(&world, 20, 0, 0);
    assert!((s.ps.origin[2] - 60.0).abs() < 1.0, "{:?}", s.ps.origin);
    near(
        "VaultOnto 0.5 s",
        s.secs_in(MecMode::LedgeClimb),
        0.5,
        DT * 1.5,
    );
}

// ---------------------------------------------------------------- vault

fn vault_world(height: f32, depth: f32) -> BoxWorld {
    BoxWorld::floor().with([300.0, -500.0, 0.0], [300.0 + depth, 500.0, height])
}

fn vault_run(world: &BoxWorld, speed: f32) -> Sim {
    let mut s = Sim::new([0.0, 0.0, 0.125], [speed, 0.0, 0.0], 0.0);
    s.mec.momentum = 1.0;
    s.run(world, 70, 127, 0);
    s
}

#[test]
fn vault_over_uses_the_reach_rule_root_motion_and_exit_speed() {
    let h = m(0.9);
    let world = vault_world(h, 16.0);
    let s = vault_run(&world, t().run_max_speed);
    assert!(
        s.events().contains(&MecEvent::VaultStart { onto: false }),
        "{:?}",
        s.events()
    );
    let i = s.first(MecMode::Vault).unwrap();
    let (p0, m0, _) = s.log[i];
    assert_eq!(m0.wall_side, script_kind::VAULT_OVER);
    let speed = hspeed_at(&s, i - 1);
    let reach = t().vault_reach(h, speed);
    let face = 300.0 - p0.origin[0];
    assert!(
        face <= reach && face > reach - speed * DT - 2.0,
        "vault starts at the reach: face {face}, reach {reach}"
    );
    near("vault time", s.secs_in(MecMode::Vault), 0.6, DT * 1.5);
    let last = s.last(MecMode::Vault).unwrap();
    let travel = s.log[last].0.origin[0] - p0.origin[0];
    near("root motion 3.7 m", travel, m(3.7), m(0.25));
    near("exit 6.5 m/s cap", hspeed_at(&s, last + 1), m(6.5), 1.0);
    assert!(s.ps.origin[0] > 316.0 + 15.0, "past it: {:?}", s.ps.origin);
    assert_eq!(s.mec.mode, MecMode::Ground);
}

#[test]
fn deep_obstacle_takes_the_long_vault() {
    let world = vault_world(m(0.9), m(2.0));
    let s = vault_run(&world, t().run_max_speed);
    let i = s.first(MecMode::Vault).expect("vaulted");
    assert_eq!(s.log[i].1.wall_side, script_kind::VAULT_OVER_LONG);
    near("long vault time", s.secs_in(MecMode::Vault), 0.75, DT * 1.5);
    assert!(s.ps.origin[0] > 300.0 + m(2.0) + 15.0, "{:?}", s.ps.origin);
}

#[test]
fn vault_limits_and_slow_vaults() {
    // 1.75 m: too high to vault over.
    let s = vault_run(&vault_world(m(1.75), 16.0), t().run_max_speed);
    assert!(s.first(MecMode::Vault).is_none());
    // 1.6 m: vaultable.
    let s = vault_run(&vault_world(m(1.6), 16.0), t().run_max_speed);
    assert!(s.first(MecMode::Vault).is_some());
    // No minimum speed: a walking start still vaults.
    let mut s = Sim::new([200.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.run(&vault_world(m(0.9), 16.0), 60, 127, 0);
    assert!(s.first(MecMode::Vault).is_some(), "slow vault");
    let exit = hspeed_at(&s, s.last(MecMode::Vault).unwrap() + 1);
    assert!(exit >= m(4.0) - 0.5 && exit <= m(6.5) + 0.5, "exit {exit}");
    // Deep platform: vault onto it.
    let s = vault_run(&vault_world(m(0.9), 600.0), t().run_max_speed);
    assert!(s.events().contains(&MecEvent::VaultStart { onto: true }));
    near("on top", s.ps.origin[2], m(0.9), 1.0);
}

#[test]
fn no_vault_over_a_death_drop() {
    // Roof parapet with a 12 m drop behind it: not vaulted (death height).
    let world = BoxWorld {
        boxes: std::vec![
            ([-2000.0, -2000.0, -64.0], [316.0, 2000.0, 0.0]),
            ([300.0, -2000.0, 0.0], [316.0, 2000.0, m(0.9)]),
            (
                [-4000.0, -4000.0, -m(12.0) - 64.0],
                [4000.0, 4000.0, -m(12.0)]
            ),
        ],
    };
    let s = vault_run(&world, t().run_max_speed);
    assert!(s.first(MecMode::Vault).is_none(), "{:?}", s.events());
    // 6 m drop behind: vaulted.
    let world = BoxWorld {
        boxes: std::vec![
            ([-2000.0, -2000.0, -64.0], [316.0, 2000.0, 0.0]),
            ([300.0, -2000.0, 0.0], [316.0, 2000.0, m(0.9)]),
            (
                [-4000.0, -4000.0, -m(6.0) - 64.0],
                [4000.0, 4000.0, -m(6.0)]
            ),
        ],
    };
    let s = vault_run(&world, t().run_max_speed);
    assert!(s.first(MecMode::Vault).is_some());
}

#[test]
fn springboard_needs_a_point_nine_to_one_point_five_metre_obstacle() {
    for (h, sb) in [(m(1.0), true), (m(0.6), false)] {
        let world = vault_world(h, 16.0);
        let mut s = Sim::new([300.0 - 61.0, 0.0, 0.125], [m(6.0), 0.0, 0.0], 0.0);
        s.mec.momentum = 1.0;
        s.step(&world, 127, buttons::JUMP);
        assert_eq!(s.events().contains(&MecEvent::Springboard), sb, "{h}");
    }
}

#[test]
fn coil_lifts_point_six_four_metres() {
    let world = BoxWorld::floor();
    let mut s = Sim::new([0.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 0, 0);
    s.step(&world, 0, buttons::JUMP);
    let r = s.step(&world, 0, buttons::CROUCH);
    assert!(s.mec.coil);
    near("coil", r.mins[2], m(0.64), 1e-3);
    assert!(s.events().contains(&MecEvent::CoilStart));
}

// -------------------------------------------------------------- landing

/// Fall from `height` (the jump start); crouch held while the origin is
/// below `.0` and above `.1`.
fn drop_test(height: f32, crouch: Option<(f32, f32)>) -> Sim {
    let world = BoxWorld::floor();
    let mut s = Sim::in_air([0.0, 0.0, height], [250.0, 0.0, 0.0], 0.0);
    for _ in 0..200 {
        let c = crouch
            .is_some_and(|(below, release)| s.ps.origin[2] < below && s.ps.origin[2] > release);
        s.step(&world, 0, if c { buttons::CROUCH } else { 0 });
    }
    s
}

const HOLD: Option<(f32, f32)> = Some((90.0, -1.0));

fn damage(s: &Sim) -> i32 {
    s.log.iter().map(|(_, _, r)| r.fall_damage).sum()
}

#[test]
fn landing_tiers_by_drop() {
    // 1.5 m: plain landing, speed kept.
    let s = drop_test(m(1.5), None);
    assert!(
        s.events()
            .iter()
            .any(|e| matches!(e, MecEvent::Land { .. }))
    );
    assert_eq!(s.ticks_in(MecMode::HardLanding), 0);
    // 3 m: stumble 0.63 s, no damage, slowed to walking.
    let s = drop_test(m(3.0), None);
    let i = s.first(MecMode::HardLanding).expect("stumble");
    assert_eq!(s.log[i].1.wall_side, script_kind::STUMBLE);
    near(
        "stumble",
        s.secs_in(MecMode::HardLanding),
        38.0 * TICK,
        DT * 1.5,
    );
    assert_eq!(damage(&s), 0);
    assert!(hspeed_at(&s, i) <= m(2.0) + 0.1);
    // 5 m: medium fail, control back at 122 t, hurt but alive.
    let s = drop_test(m(5.0), None);
    let i = s.first(MecMode::HardLanding).unwrap();
    assert_eq!(s.log[i].1.wall_side, script_kind::FAIL_MEDIUM);
    near(
        "fail medium",
        s.secs_in(MecMode::HardLanding),
        122.0 * TICK,
        DT * 1.5,
    );
    let d = damage(&s);
    assert!((20..=35).contains(&d), "damage {d}");
    assert!(hspeed_at(&s, i) < 1.0);
    // 8 m: fail, control back at 177 t.
    let s = drop_test(m(8.0), None);
    let i = s.first(MecMode::HardLanding).unwrap();
    assert_eq!(s.log[i].1.wall_side, script_kind::FAIL);
    near(
        "fail",
        s.secs_in(MecMode::HardLanding),
        177.0 * TICK,
        DT * 1.5,
    );
    let d = damage(&s);
    assert!((35..100).contains(&d), "damage {d}");
}

#[test]
fn roll_needs_crouch_at_touchdown_and_a_metre_of_drop() {
    let s = drop_test(m(3.0), HOLD);
    let ev = s.events();
    assert!(
        ev.iter().any(|e| matches!(e, MecEvent::Roll { .. })),
        "{ev:?}"
    );
    assert!(!ev.iter().any(|e| matches!(e, MecEvent::HardLanding { .. })));
    assert_eq!(damage(&s), 0);
    // FallingLandRoll: control back at the 1.0 s move-out.
    near("roll move-out", s.secs_in(MecMode::Roll), 1.0, DT * 1.5);
    let a = s.first(MecMode::Roll).unwrap();
    let b = s.last(MecMode::Roll).unwrap();
    // Momentum carried: a 6.35 m/s landing rolls further than the clip's own
    // 3.8 m by the move-out, and never stalls on the way.
    let dist = s.log[b].0.origin[0] - s.log[a].0.origin[0];
    assert!(dist > m(4.4) && dist < m(6.2), "roll travel {dist}");
    let slowest = (a..=b).map(|i| hspeed_at(&s, i)).fold(f32::MAX, f32::min);
    assert!(slowest > m(4.4), "slowest roll speed {slowest}");
    near(
        "exit speed",
        hspeed_at(&s, b + 1),
        m(LAND_ROLL.forward_speed(1.0)),
        m(0.3),
    );
    // Crouch pressed early but released before touchdown: no roll.
    let s = drop_test(m(3.0), Some((200.0, 30.0)));
    assert!(
        !s.events()
            .iter()
            .any(|e| matches!(e, MecEvent::Roll { .. }))
    );
    // 0.8 m drop: never a roll.
    let s = drop_test(m(0.8), HOLD);
    assert!(
        !s.events()
            .iter()
            .any(|e| matches!(e, MecEvent::Roll { .. }))
    );
    // Roll also saves a 7 m drop.
    let s = drop_test(m(7.0), HOLD);
    assert!(
        s.events()
            .iter()
            .any(|e| matches!(e, MecEvent::Roll { .. }))
    );
    assert_eq!(damage(&s), 0);
}

#[test]
fn lethal_fall_kills_even_with_roll() {
    let s = drop_test(m(10.5), HOLD);
    assert!(s.log.iter().any(|(_, _, r)| r.fall_damage >= 1000));
    assert!(
        !s.events()
            .iter()
            .any(|e| matches!(e, MecEvent::Roll { .. }))
    );
    // 9.5 m without a roll hurts but does not kill.
    let s = drop_test(m(9.5), None);
    assert!(damage(&s) < 100);
}

#[test]
fn a_flat_jump_is_never_a_heavy_landing() {
    let world = BoxWorld::floor();
    let mut s = Sim::new([0.0, 0.0, 0.125], [m(7.2), 0.0, 0.0], 0.0);
    s.mec.momentum = 1.0;
    s.step(&world, 127, buttons::JUMP);
    s.run(&world, 40, 127, buttons::CROUCH);
    assert!(
        !s.events()
            .iter()
            .any(|e| matches!(e, MecEvent::HardLanding { .. } | MecEvent::Roll { .. }))
    );
}

#[test]
fn deterministic_replay() {
    let world = BoxWorld::floor().with([-200.0, 40.0, 0.0], [3200.0, 120.0, 400.0]);
    let go = || {
        let r = 15.0_f32.to_radians();
        let mut s = Sim::new(
            [0.0, 0.0, 0.125],
            [280.0 * r.cos(), 280.0 * r.sin(), 0.0],
            15.0,
        );
        s.run(&world, 70, 127, buttons::JUMP);
        (s.ps, s.mec)
    };
    let (a, am) = go();
    let (b, bm) = go();
    assert_eq!(a.origin.map(f32::to_bits), b.origin.map(f32::to_bits));
    assert_eq!(a.velocity.map(f32::to_bits), b.velocity.map(f32::to_bits));
    assert_eq!(am, bm);
}

fn hspeed_at(s: &Sim, i: usize) -> f32 {
    let v = s.log[i].0.velocity;
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}

/// Roof edge whose rounded-hull contact reads as a steep bevel: every hull
/// trace that stops on the roof while the body's centre is past the edge
/// (x > 0) reports the bevel normal, as IW4's capsule does on a mesh edge.
/// Thin probes see the true faces.
struct BevelEdge {
    boxes: BoxWorld,
}

impl CollisionBackend for BevelEdge {
    fn trace(&self, i: GroundTraceInput) -> Trace {
        let mut t = self.boxes.trace(i);
        let wide = i.maxs[0] - i.mins[0] > 4.0;
        if wide && t.fraction < 1.0 && t.normal[2] > 0.9 && t.endpos[0] > 0.0 {
            t.normal = [0.79, 0.0, 0.61];
        }
        t
    }
}

fn bevel_roof() -> BevelEdge {
    // Roof top at z 0 for x < 0; the street is 20 m below.
    BevelEdge {
        boxes: BoxWorld {
            boxes: std::vec![
                ([-2000.0, -4000.0, -400.0], [0.0, 4000.0, 0.0]),
                ([-4000.0, -4000.0, -900.0], [4000.0, 4000.0, -800.0]),
            ],
        },
    }
}

#[test]
fn walking_along_a_bevelled_roof_edge_keeps_footing() {
    // Centre 9 in past the edge, running parallel to it: the footprint still
    // has roof under it, so the body stays on the roof (IW4 capsule: ~10.5).
    let world = bevel_roof();
    let mut s = Sim::new([9.0, 0.0, 0.125], [0.0; 3], 90.0);
    s.run(&world, 60, 127, 0);
    assert_eq!(s.mec.mode, MecMode::Ground, "{:?}", s.ps.origin);
    assert!(
        s.ps.origin[2] > -1.0,
        "still on the roof: {:?}",
        s.ps.origin
    );
    assert!(
        s.ps.origin[1] > 150.0,
        "ran along the edge: {:?}",
        s.ps.origin
    );
    let airborne = s
        .log
        .iter()
        .filter(|(_, m, _)| m.mode == MecMode::Air)
        .count();
    assert!(airborne <= 1, "no ground/air flicker: {airborne} air ticks");
}

#[test]
fn hull_past_the_footprint_is_not_supported() {
    let world = bevel_roof();
    let mut s = Sim::new([14.0, 0.0, 0.125], [0.0; 3], 90.0);
    // The box world cannot tilt the hull off the edge; the body must at least
    // not stand on a contact that is only the bevel.
    s.run(&world, 10, 0, 0);
    assert_eq!(s.mec.mode, MecMode::Air, "{:?}", s.ps.origin);
}

#[test]
fn ground_held_at_the_footprint_limit_does_not_flicker() {
    // 11.5 in out: too far to land on (0.7 of the half width) but a runner
    // already on the roof keeps it (0.85), so walking the edge never flips.
    let world = bevel_roof();
    let mut s = Sim::new([5.0, 0.0, 0.125], [0.0; 3], 90.0);
    s.run(&world, 5, 0, 0);
    assert_eq!(s.mec.mode, MecMode::Ground);
    s.ps.origin[0] = 11.5;
    s.run(&world, 45, 127, 0);
    let airborne = s
        .log
        .iter()
        .filter(|(_, m, _)| m.mode == MecMode::Air)
        .count();
    assert_eq!(s.mec.mode, MecMode::Ground, "{:?}", s.ps.origin);
    assert!(airborne <= 1, "{airborne} air ticks");
}

/// Counts traces; the movement must stay within a fixed trace budget per
/// command whatever the world (no unbounded probe / retry loop).
struct Counting<'a> {
    inner: &'a BoxWorld,
    n: std::cell::Cell<u32>,
}

impl CollisionBackend for Counting<'_> {
    fn trace(&self, i: GroundTraceInput) -> Trace {
        self.n.set(self.n.get() + 1);
        self.inner.trace(i)
    }
}

#[test]
fn every_command_runs_a_bounded_number_of_traces() {
    // A cluttered world: walls, ledges, boxes, a pit; every move gets tried.
    let mut world = BoxWorld::floor()
        .with([-200.0, 40.0, 0.0], [3200.0, 120.0, 400.0])
        .with([600.0, -500.0, 0.0], [616.0, 30.0, 36.0])
        .with([900.0, -500.0, 0.0], [1300.0, 30.0, 150.0]);
    for i in 0..40 {
        let x = 1400.0 + i as f32 * 37.0;
        world = world.with(
            [x, -300.0, 0.0],
            [x + 9.0, 20.0, 10.0 + (i % 7) as f32 * 9.0],
        );
    }
    let mut worst = 0;
    for (msec, buttons_seq) in [(33, 0_u32), (200, 1), (200, 2)] {
        let mut s = Sim::new([0.0, 0.0, 0.125], [m(7.2), 0.0, 0.0], 3.0);
        s.mec.momentum = 1.0;
        for k in 0..240 {
            let held = match (buttons_seq + k / 7) % 4 {
                0 => buttons::JUMP,
                1 => buttons::CROUCH,
                2 => MecContext::QUICKTURN_BUTTON_DEFAULT,
                _ => 0,
            };
            s.time += msec;
            let mut cmd = UserCmd {
                server_time: s.time,
                buttons: held,
                forwardmove: 127,
                ..UserCmd::default()
            };
            cmd.angles[1] = (s.yaw * ANGLE2SHORT) as i32 & 0xffff;
            let counted = Counting {
                inner: &world,
                n: std::cell::Cell::new(0),
            };
            let ctx = MecContext::new(msec);
            mec_pmove(&mut s.ps, &mut s.mec, &cmd, &ctx, &counted);
            worst = worst.max(counted.n.get() * 33 / msec.max(1) as u32);
        }
    }
    // Per 33 ms of movement (a 200 ms command is ~6 substeps).
    assert!(worst <= 160, "worst traces per 33 ms substep: {worst}");
}

// ------------------------------------------------- vault onto / springboard

/// Run at `speed` from `x0` (momentum matching the speed) at a box whose face
/// is at x = 300, with `press` = log index of a one-tick jump press.
fn run_at_box(world: &BoxWorld, speed: f32, x0: f32, press: Option<usize>, ticks: usize) -> Sim {
    let mut s = Sim::new([x0, 0.0, 0.125], [speed, 0.0, 0.0], 0.0);
    s.mec.momentum = t().run_time_for(speed) / t().run_curve_time;
    for i in 0..ticks {
        let held = if press == Some(i) { buttons::JUMP } else { 0 };
        s.step(world, 127, held);
    }
    s
}

#[test]
fn running_vault_onto_reaches_the_top_from_0_9_to_1_7_metres() {
    let hw = 15.0;
    for h in [0.9_f32, 1.0, 1.2, 1.5, 1.65, 1.7] {
        for v in [2.0_f32, 4.0, 5.5, 7.2] {
            for phase in 0..4 {
                // Deep top: vault onto it and stay there.
                let world = vault_world(m(h), 600.0);
                let s = run_at_box(&world, m(v), phase as f32 * 3.0, None, 90);
                let what = std::format!("h {h} v {v} phase {phase}");
                assert!(
                    s.events().contains(&MecEvent::VaultStart { onto: true }),
                    "{what}: {:?}",
                    s.events()
                );
                // The whole VaultOnto clip plays (no early end), and the body
                // never drives into the face below the lip.
                near(&what, s.secs_in(MecMode::Vault), 0.5, DT * 1.5);
                for w in s.log.windows(2) {
                    let ((p0, m0, _), (p1, mm, _)) = (&w[0], &w[1]);
                    if m0.mode != MecMode::Vault
                        || mm.mode != MecMode::Vault
                        || mm.mode_ms >= mm.move_ms
                    {
                        continue;
                    }
                    // Scripted velocity = the path step; any shortfall is the
                    // hull blocked by the obstacle.
                    let moved = p1.origin[0] - p0.origin[0];
                    let want = p1.velocity[0] * DT;
                    assert!(
                        (moved - want).abs() < 0.5,
                        "{what}: path blocked at {:?} ({moved} of {want})",
                        p1.origin
                    );
                    if p1.origin[2] < m(h) - 0.5 {
                        assert!(
                            p1.origin[0] + hw <= 300.0 + 0.5,
                            "{what}: into the face {:?}",
                            p1.origin
                        );
                    }
                }
                let fell = s
                    .log
                    .iter()
                    .skip(s.first(MecMode::Vault).unwrap())
                    .any(|(_, mm, _)| mm.mode == MecMode::Air);
                assert!(!fell, "{what}: fell off after the vault");
                assert_eq!(s.mec.mode, MecMode::Ground, "{what}");
                near(&what, s.ps.origin[2], m(h), 1.0);
                assert!(s.ps.origin[0] > 300.0 + hw, "{what}: {:?}", s.ps.origin);

                // Shallow top (0.4 m, drop behind): vault over it (depth rule).
                let world = vault_world(m(h), 16.0);
                let s = run_at_box(&world, m(v), phase as f32 * 3.0, None, 90);
                assert!(
                    s.events().contains(&MecEvent::VaultStart { onto: false }),
                    "{what} over: {:?}",
                    s.events()
                );
                assert_eq!(
                    s.log[s.first(MecMode::Vault).unwrap()].1.wall_side,
                    script_kind::VAULT_OVER
                );
                assert!(
                    s.ps.origin[0] > 316.0 + hw,
                    "{what} over: {:?}",
                    s.ps.origin
                );
                assert!(s.ps.origin[2] < 1.0, "{what} over: {:?}", s.ps.origin);
            }
        }
    }
}

#[test]
fn springboard_takes_a_buffered_jump_with_two_ticks_of_jitter() {
    for (h, depth) in [
        (1.0_f32, 16.0_f32),
        (1.2, 16.0),
        (1.5, 16.0),
        (1.0, 600.0),
        (1.4, 600.0),
    ] {
        for v in [6.0_f32, 7.2] {
            let world = vault_world(m(h), depth);
            // No jump: the auto-vault.
            let base = run_at_box(&world, m(v), 0.0, None, 90);
            let k_vault = base
                .first(MecMode::Vault)
                .expect("auto-vault without a jump");
            assert!(!base.events().contains(&MecEvent::Springboard));
            // Nominal presses from 9 ticks (0.3 s) before the vault would
            // start to the tick it starts, each ±2 ticks.
            for lead in [0_usize, 3, 6, 9] {
                for jitter in -2_i32..=2 {
                    let press = (k_vault as i32 - lead as i32 + jitter) as usize;
                    let s = run_at_box(&world, m(v), 0.0, Some(press), 90);
                    let what =
                        std::format!("h {h} depth {depth} v {v} lead {lead} jitter {jitter}");
                    let ev = s.events();
                    assert!(ev.contains(&MecEvent::Springboard), "{what}: {ev:?}");
                    assert!(!ev.contains(&MecEvent::Jump), "{what}: plain jump {ev:?}");
                    if depth < 100.0 {
                        assert!(
                            s.ps.origin[0] > 300.0 + depth + 15.0,
                            "{what}: {:?}",
                            s.ps.origin
                        );
                    } else {
                        near(&what, s.ps.origin[2], m(h), 1.0);
                    }
                    // Apex 1.7 m or more above the take-off ground, at most
                    // 2.8 m above the obstacle top (the lip launch).
                    let apex = s
                        .log
                        .iter()
                        .map(|(p, _, _)| p.origin[2])
                        .fold(f32::MIN, f32::max);
                    assert!(
                        apex > m(1.7) - 2.0 && apex < m(h + 2.8),
                        "{what}: apex {apex}"
                    );
                }
            }
            // A press held for several ticks behaves the same.
            let mut s = Sim::new([0.0, 0.0, 0.125], [m(v), 0.0, 0.0], 0.0);
            s.mec.momentum = t().run_time_for(m(v)) / t().run_curve_time;
            for i in 0..90 {
                let held = if (k_vault - 5..k_vault + 1).contains(&i) {
                    buttons::JUMP
                } else {
                    0
                };
                s.step(&world, 127, held);
            }
            assert!(
                s.events().contains(&MecEvent::Springboard),
                "held press: {:?}",
                s.events()
            );
        }
    }
    // A jump pressed long before the window is a plain jump.
    let world = vault_world(m(1.2), 16.0);
    let s = run_at_box(&world, m(7.2), -400.0, Some(2), 90);
    let ev = s.events();
    assert_eq!(ev.first(), Some(&MecEvent::Jump), "{ev:?}");
    // Too slow for a springboard: the press is a jump at once, never held.
    let s = run_at_box(&world, m(4.0), 150.0, Some(1), 3);
    assert!(s.events().contains(&MecEvent::Jump), "{:?}", s.events());
    // Walking off an edge while the press is held for a springboard: jump off it.
    let world = BoxWorld {
        boxes: std::vec![
            ([-2000.0, -2000.0, -64.0], [150.0, 2000.0, 0.0]),
            ([-2000.0, -2000.0, -264.0], [2000.0, 2000.0, -200.0]),
            ([300.0, -2000.0, -200.0], [316.0, 2000.0, m(1.2)]),
        ],
    };
    let s = run_at_box(&world, m(7.2), 0.0, Some(0), 30);
    assert!(s.events().contains(&MecEvent::Jump), "{:?}", s.events());
}

#[test]
fn unlocks_default_to_the_full_ffa_move_set() {
    let base = MecTuning::BASE;
    let ffa = MecTuning::DEFAULT;
    assert_eq!(base.unlocks, MecUnlocks::NONE);
    assert_eq!(ffa.unlocks, MecUnlocks::ALL);
    assert_eq!(
        (
            base.wallruns_per_air,
            base.wallclimbs_per_air,
            base.wall_moves_per_air
        ),
        (1, 1, 2)
    );
    assert_eq!(
        (
            ffa.wallruns_per_air,
            ffa.wallclimbs_per_air,
            ffa.wall_moves_per_air
        ),
        (2, 2, 3)
    );
    near("base slide", base.slide_max_time, 3.0, 0.0);
    near("long slide", ffa.slide_max_time, 5.0, 0.0);
    assert_eq!(MecContext::new(16).tuning, MecTuning::DEFAULT);
    near("align", ffa.wallrun_align_time, 16.0 * TICK, 1e-6);
    near("sighting", ffa.springboard_sighting_time, 0.5, 0.0);

    // Long slide: 5 s at most, and the slide curve stretches over it.
    let world = BoxWorld::floor();
    let mut s = Sim::new([0.0, 0.0, 0.125], [m(10.0), 0.0, 0.0], 0.0);
    s.tuning = ffa;
    s.tuning.slide_min_speed = 0.0;
    s.mec.momentum = 1.0;
    s.step(&world, 0, buttons::CROUCH);
    s.run(&world, 190, 0, buttons::CROUCH);
    near("long slide max time", s.secs_in(MecMode::Slide), 5.0, 0.07);
    let mut s = Sim::new([0.0, 0.0, 0.125], [m(7.2), 0.0, 0.0], 0.0);
    s.tuning = ffa;
    s.mec.momentum = 1.0;
    s.step(&world, 127, 0);
    s.run(&world, 90, 127, buttons::CROUCH);
    near("long slide 7.2 → 4", s.secs_in(MecMode::Slide), 1.7, 0.1);

    // Coil and quickturn need their unlocks.
    for (u, on) in [(MecUnlocks::NONE, false), (MecUnlocks::ALL, true)] {
        let mut s = Sim::new([0.0, 0.0, 0.125], [0.0; 3], 0.0);
        s.tuning = MecTuning::BASE.with_unlocks(u);
        s.step(&world, 0, 0);
        s.step(&world, 0, buttons::JUMP);
        s.step(&world, 0, buttons::CROUCH);
        assert_eq!(s.mec.coil, on, "coil {on}");
        s.run(&world, 40, 0, 0);
        s.step(&world, 0, MecContext::QUICKTURN_BUTTON_DEFAULT);
        assert_eq!(
            s.events().contains(&MecEvent::QuickTurn),
            on,
            "quickturn {on}"
        );
    }

    // Extended wallrun: two wallclimbs in one airtime (on two walls).
    let world = BoxWorld::floor()
        .with([100.0, -500.0, 0.0], [300.0, 500.0, 1000.0])
        .with([-300.0, -500.0, 0.0], [-20.0, 500.0, 1000.0]);
    let mut s = Sim::new([75.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.tuning = ffa;
    s.step(&world, 127, 0);
    s.run(&world, 20, 127, buttons::JUMP);
    s.step(&world, 0, MecContext::QUICKTURN_BUTTON_DEFAULT);
    s.run(&world, 20, 0, 0);
    // Turned around on the wall: jump off it and climb the wall behind.
    s.step(&world, 0, buttons::JUMP);
    s.run(&world, 30, 127, buttons::JUMP);
    let climbs = s
        .events()
        .iter()
        .filter(|e| **e == MecEvent::WallClimbStart)
        .count();
    assert_eq!(climbs, 2, "{:?}", s.events());
}

/// Corridor of `width` between two 10 m walls along x (faces at y = ±width/2).
fn corridor(width: f32) -> BoxWorld {
    let half = width / 2.0;
    BoxWorld::floor()
        .with([-200.0, half, 0.0], [4000.0, half + 80.0, 400.0])
        .with([-200.0, -half - 80.0, 0.0], [4000.0, -half, 400.0])
}

/// Running jump into the left wall of a corridor, wallrun, wall jump looking
/// `look_away` degrees off the corridor axis (away from the wall), jump held.
fn corridor_run(tuning: MecTuning, width: f32, look_away: f32) -> Sim {
    let world = corridor(width);
    let half = width / 2.0;
    let r = 10.0_f32.to_radians();
    let v = t().run_max_speed;
    let mut s = Sim::in_air(
        [0.0, half - 30.0, 60.0],
        [v * r.cos(), v * r.sin(), m(4.0)],
        10.0,
    );
    s.tuning = tuning;
    s.mec.move_from = [-m(1.0), half - 40.0, 0.125];
    while s.mec.mode != MecMode::WallRun && s.log.len() < 20 {
        s.step(&world, 127, buttons::JUMP);
    }
    assert_eq!(s.mec.mode, MecMode::WallRun, "{:?}", s.events());
    s.run(&world, 8, 127, buttons::JUMP);
    s.step(&world, 127, 0);
    s.yaw = -look_away;
    s.step(&world, 127, buttons::JUMP); // wall jump
    s.run(&world, 40, 127, buttons::JUMP);
    s
}

#[test]
fn wallrun_wall_jump_wallrun_across_a_corridor() {
    for (width, look) in [(m(3.0), 0.0), (m(3.2), 0.0), (m(3.5), 30.0), (m(4.0), 45.0)] {
        let s = corridor_run(MecTuning::DEFAULT, width, look);
        let what = std::format!("corridor {} m, look {look}°", width / METRE);
        let ev = s.events();
        let starts: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                MecEvent::WallRunStart { side } => Some(*side),
                _ => None,
            })
            .collect();
        assert_eq!(starts, [-1, 1], "{what}: {ev:?}");
        assert!(ev.contains(&MecEvent::WallJump), "{what}");
        // Off the ground from the first wallrun to the end of the second.
        let a = s.first(MecMode::WallRun).unwrap();
        let b = s.last(MecMode::WallRun).unwrap();
        assert!(
            s.log[a..=b]
                .iter()
                .all(|(p, mm, _)| mm.mode != MecMode::Ground && p.origin[2] > 20.0),
            "{what}: touched the ground"
        );
        // The second wallrun is on the right wall and pulled onto it.
        let (p, mm, _) = s.log[b];
        assert!(mm.wall_normal[1] > 0.9, "{what}: {:?}", mm.wall_normal);
        assert!(
            p.origin[1] < -width / 2.0 + 15.0 + 2.0,
            "{what}: on the wall {:?}",
            p.origin
        );
    }
    // Base tuning: one wallrun per airtime, so no second wallrun.
    let s = corridor_run(t(), m(3.0), 0.0);
    let starts = s
        .events()
        .iter()
        .filter(|e| matches!(e, MecEvent::WallRunStart { .. }))
        .count();
    assert_eq!(starts, 1);
}

/// Triangle-soup collision (arena meshes): a hull that starts wholly inside a
/// solid touches no surface, so it is neither startsolid nor blocked by it.
struct Hollow<'a>(&'a BoxWorld);

impl CollisionBackend for Hollow<'_> {
    fn trace(&self, i: GroundTraceInput) -> Trace {
        let outside = BoxWorld {
            boxes: self
                .0
                .boxes
                .iter()
                .copied()
                .filter(|(bmin, bmax)| {
                    !(0..3).all(|a| {
                        i.start[a] + i.mins[a] > bmin[a] + TOUCH
                            && i.start[a] + i.maxs[a] < bmax[a] - TOUCH
                    })
                })
                .collect(),
        };
        outside.trace(i)
    }
}

#[test]
fn step_in_front_of_a_wall_is_vaulted_onto_on_mesh_collision() {
    // 1.2 m step, 3 m deep, a 4.8 m wall behind it (bastion step); and a
    // 0.9 m step 1.5 m deep before a 3 m wall.
    for (h, depth, wall) in [(1.2_f32, 3.0_f32, 4.8_f32), (0.9, 1.5, 3.0)] {
        let world = BoxWorld::floor()
            .with([300.0, -500.0, 0.0], [300.0 + m(depth), 500.0, m(h)])
            .with(
                [300.0 + m(depth), -500.0, 0.0],
                [300.0 + m(depth) + 200.0, 500.0, m(wall)],
            );
        for v in [3.0_f32, 7.2] {
            let mut s = Sim::new([0.0, 0.0, 0.125], [m(v), 0.0, 0.0], 0.0);
            s.mec.momentum = t().run_time_for(m(v)) / t().run_curve_time;
            for _ in 0..90 {
                s.step(&Hollow(&world), 127, 0);
            }
            let what = std::format!("h {h} depth {depth} v {v}");
            assert!(
                s.events().contains(&MecEvent::VaultStart { onto: true }),
                "{what}: {:?}",
                s.events()
            );
            near(&what, s.ps.origin[2], m(h), 1.0);
            assert_eq!(s.mec.mode, MecMode::Ground, "{what}");
        }
    }
}

// ------------------------------------------------------- move continuity
/// Largest per-tick change of the velocity (horizontal, vertical) over the
/// ticks `a..=b` of the log (each compared with the tick before it).
fn max_dv(s: &Sim, a: usize, b: usize) -> (f32, f32) {
    let mut h = 0.0_f32;
    let mut z = 0.0_f32;
    for i in a.max(1)..=b.min(s.log.len() - 1) {
        let (p, q) = (s.log[i - 1].0.velocity, s.log[i].0.velocity);
        h = h.max(((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt());
        z = z.max((q[2] - p[2]).abs());
    }
    (h, z)
}

/// Per-tick velocity change (in/s per 33 ms command) allowed across a move's
/// first and last tick: about 1 m/s, a hard sprint stop takes several ticks.
const SEAM_DV: f32 = 40.0;

fn seam_dv(s: &Sim, i: usize) -> f32 {
    let (p, q) = (s.log[i - 1].0.velocity, s.log[i].0.velocity);
    ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt()
}

#[test]
fn roll_carries_momentum_without_a_stall_or_a_lurch() {
    let world = BoxWorld::floor();
    for entry in [m(4.0), m(6.35), t().run_max_speed] {
        let mut s = Sim::in_air([0.0, 0.0, m(3.0)], [entry, 0.0, 0.0], 0.0);
        for _ in 0..120 {
            let c = s.ps.origin[2] < 90.0 && s.mec.mode != MecMode::Ground;
            s.step(&world, 127, if c { buttons::CROUCH } else { 0 });
        }
        let a = s.first(MecMode::Roll).expect("rolled");
        let b = s.last(MecMode::Roll).unwrap();
        let landing = hspeed_at(&s, a - 1);
        let exit = m(LAND_ROLL.forward_speed(t().roll_move_out));
        // No stall: the old root-motion drive dipped to ~25% of the entry.
        let slowest = (a..=b + 3).map(|i| hspeed_at(&s, i)).fold(f32::MAX, f32::min);
        assert!(
            slowest > 0.8 * landing.min(exit),
            "entry {entry}: slowest {slowest} (landing {landing}, exit {exit})"
        );
        // Continuous through the roll and both seams (landing tick, control back).
        let (dv, _) = max_dv(&s, a, b + 3);
        assert!(dv < 15.0, "entry {entry}: per-tick dv {dv}");
        near("exit speed", hspeed_at(&s, b + 1), exit, 2.0);
    }
}

#[test]
fn vault_blends_momentum_at_both_seams() {
    let world = vault_world(m(0.9), 16.0);
    for speed in [m(4.0), t().run_max_speed] {
        let s = vault_run(&world, speed);
        let a = s.first(MecMode::Vault).expect("vaulted");
        let b = s.last(MecMode::Vault).unwrap();
        let entry = hspeed_at(&s, a - 1);
        assert!(seam_dv(&s, a + 1) < SEAM_DV, "start dv {}", seam_dv(&s, a + 1));
        assert!(seam_dv(&s, b + 1) < SEAM_DV, "end dv {}", seam_dv(&s, b + 1));
        let slowest = (a..=b).map(|i| hspeed_at(&s, i)).fold(f32::MAX, f32::min);
        assert!(slowest > 0.6 * entry, "speed {speed}: slowest {slowest} of {entry}");
    }
}

#[test]
fn ledge_climb_hands_back_its_exit_speed_smoothly() {
    let world = BoxWorld::floor().with([100.0, -500.0, 0.0], [400.0, 500.0, 150.0]);
    let mut s = Sim::new([75.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 127, 0);
    s.run(&world, 90, 127, buttons::JUMP);
    let b = s.last(MecMode::LedgeClimb).expect("climbed");
    assert!(seam_dv(&s, b + 1) < SEAM_DV, "end dv {}", seam_dv(&s, b + 1));
    let vz = (s.log[b].0.velocity[2] - s.log[b + 1].0.velocity[2]).abs();
    assert!(vz < SEAM_DV, "end dvz {vz}");
}

#[test]
fn root_curves_have_continuous_velocity() {
    for c in [
        crate::rootmotion::VAULT_OVER_FAST,
        crate::rootmotion::VAULT_ONTO,
        VAULT_ONTO_HIGH,
        LAND_ROLL,
    ] {
        // Keys are still hit exactly.
        for k in c.keys {
            let (up, fwd) = c.at(k.0);
            assert!((up - k.1).abs() < 1e-4 && (fwd - k.2).abs() < 1e-4);
        }
        // No velocity step at any interior key (straight lines between the
        // 15 Hz keys stepped by up to 2.4 m/s there).
        let h = 1.0e-3;
        for k in &c.keys[1..c.keys.len() - 1] {
            for ch in [0, 1] {
                let p = |t: f32| if ch == 0 { c.at(t).0 } else { c.at(t).1 };
                let before = (p(k.0) - p(k.0 - h)) / h;
                let after = (p(k.0 + h) - p(k.0)) / h;
                assert!((after - before).abs() < 0.1, "step {before} -> {after} at {}", k.0);
            }
        }
    }
}

#[test]
fn quickturn_eases_in_and_out() {
    let world = BoxWorld::floor();
    let mut s = Sim::new([0.0, 0.0, 0.125], [0.0; 3], 30.0);
    s.step(&world, 0, 0);
    s.step(&world, 0, MecContext::QUICKTURN_BUTTON_DEFAULT);
    s.run(&world, 18, 0, 0);
    let yaw: Vec<f32> = s.log.iter().map(|(p, _, _)| p.viewangles[1]).collect();
    let steps: Vec<f32> = yaw
        .windows(2)
        .map(|w| {
            let mut d = w[1] - w[0];
            while d < -180.0 {
                d += 360.0;
            }
            d
        })
        .filter(|d| *d > 1.0e-3)
        .collect();
    let mean = 180.0 / steps.len() as f32;
    assert!(steps[0] < 0.5 * mean, "first step {} of mean {mean}", steps[0]);
    assert!(*steps.last().unwrap() < 0.5 * mean, "last step {steps:?}");
    for w in steps.windows(2) {
        assert!((w[1] - w[0]).abs() < 0.5 * mean, "{steps:?}");
    }
}

#[test]
fn weapon_comes_up_over_the_end_of_a_move() {
    // Roll: lowered while rolling, raised over the last 250 ms, fire still off.
    let s = drop_test(m(3.0), HOLD);
    let rolls: Vec<_> = s
        .log
        .iter()
        .filter(|(_, mm, _)| mm.mode == MecMode::Roll)
        .collect();
    assert!(rolls.first().unwrap().2.gates.lowered);
    let last = rolls.last().unwrap();
    assert!(!last.2.gates.lowered && !last.2.gates.allow_fire);
    let raised = rolls.iter().filter(|(_, _, r)| !r.gates.lowered).count() as f32 * DT;
    assert!(raised > 0.2 && raised < 0.32, "raised for {raised} s");
}

/// IW4 view-height bookkeeping the sim runs after each move (`mec_pmove_iw4`).
fn view_height_step(s: &mut Sim) {
    let cmd = UserCmd {
        server_time: s.time,
        ..UserCmd::default()
    };
    let pml = movement_iw4::Pml {
        forward: [0.0; 3],
        right: [0.0; 3],
        up: [0.0; 3],
        frametime: MSEC as f32 * 0.001,
        msec: MSEC,
        walking: 1,
        ground_plane: 1,
        almost_ground_plane: 0,
        ground_trace: [0; 11],
        previous_origin: s.ps.origin,
        previous_velocity: s.ps.velocity,
        holdrand: 0,
        jump_animations: [None; 4],
        mantle_movetype: None,
        landing_animation: false,
    };
    let _ = movement_iw4::update_stance_target(&mut s.ps);
    movement_iw4::update_view_height(&mut s.ps, &pml, &cmd);
}

#[test]
fn eye_height_eases_back_up_after_a_roll() {
    let world = BoxWorld::floor();
    let mut s = Sim::in_air([0.0, 0.0, m(3.0)], [250.0, 0.0, 0.0], 0.0);
    s.ps.view_height_target = 60;
    s.ps.view_height_current = 60.0;
    let mut vh = Vec::new();
    for _ in 0..90 {
        let c = s.ps.origin[2] < 90.0 && s.mec.mode != MecMode::Ground;
        s.step(&world, 127, if c { buttons::CROUCH } else { 0 });
        view_height_step(&mut s);
        vh.push(s.ps.view_height_current);
    }
    assert!(vh.iter().any(|v| (*v - 40.0).abs() < 0.5), "crouched in the roll");
    assert!((vh.last().unwrap() - 60.0).abs() < 0.5, "standing after");
    // 20 in over the 200 ms stance lerp: ≤ ~1/5 of it per 33 ms command.
    let dv = vh.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
    assert!(dv < 7.0, "view height step {dv}");
}

#[test]
fn vault_over_a_tall_box_steps_down_instead_of_dropping() {
    // 1.4 m box, 0.5 m deep: the clip starts down while the body is still
    // over the top; leaving the far edge must not fall the gap in one tick.
    for (h, d) in [(m(1.4), m(0.5)), (m(0.9), 16.0), (m(1.2), m(0.3))] {
        let world = vault_world(h, d);
        let s = vault_run(&world, t().run_max_speed);
        let a = s.first(MecMode::Vault).expect("vaulted");
        let end = (a + 40).min(s.log.len() - 1);
        let worst = (a..end)
            .map(|i| s.log[i].0.origin[2] - s.log[i + 1].0.origin[2])
            .fold(0.0_f32, f32::max);
        // ~ the clip's own descent (≤ ~5 m/s → 6.5 in per 33 ms) plus gravity on exit.
        assert!(worst < 16.0, "{h}x{d}: fell {worst} in in one tick");
        assert!(s.ps.origin[0] > 300.0 + d + 15.0, "past it: {:?}", s.ps.origin);
    }
}

#[test]
fn vault_approach_does_not_flicker() {
    // Running at a vaultable box: the probes must not toggle the body between
    // ground and air, or pull its speed / height around, before the vault.
    for (h, speed) in [(m(0.9), t().run_max_speed), (m(1.4), t().run_max_speed), (m(1.2), m(4.0))] {
        let world = vault_world(h, 16.0);
        let mut s = Sim::new([0.0, 0.0, 0.125], [speed, 0.0, 0.0], 0.0);
        s.mec.momentum = 1.0;
        s.run(&world, 70, 127, 0);
        let a = s.first(MecMode::Vault).expect("vaulted");
        for i in 1..a {
            let (p, mm, _) = s.log[i];
            assert_eq!(mm.mode, MecMode::Ground, "{h}: tick {i} {:?}", mm.mode);
            assert!((p.origin[2] - 0.125).abs() < 0.01, "{h}: height {}", p.origin[2]);
            assert!(hspeed_at(&s, i) + 0.5 >= hspeed_at(&s, i - 1), "{h}: slowed at {i}");
        }
    }
}

#[test]
fn wallclimb_ledge_ground_is_continuous_twice_over() {
    // Balcony (3.8 m) then a second 3.8 m wall from its top: wallclimb →
    // ledge → run → wallclimb → ledge → run, jump held throughout.
    let world = BoxWorld::floor()
        .with([100.0, -500.0, 0.0], [1500.0, 500.0, 150.0])
        .with([260.0, -500.0, 150.0], [1500.0, 500.0, 300.0]);
    let mut s = Sim::new([75.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 127, 0);
    let mut was_climbing = false;
    let mut release = 0;
    for _ in 0..170 {
        // Let go of jump for a few ticks after the first climb, then press again.
        let climbing = s.mec.mode == MecMode::LedgeClimb;
        if was_climbing && !climbing {
            release = 4;
        }
        was_climbing = climbing;
        let near_upper = s.ps.origin[2] < 140.0 || s.ps.origin[0] > 225.0;
        let held = if release > 0 {
            release -= 1;
            0
        } else if near_upper {
            buttons::JUMP
        } else {
            0
        };
        s.step(&world, 127, held);
    }
    let climbs = s
        .log
        .windows(2)
        .filter(|w| w[0].1.mode != MecMode::LedgeClimb && w[1].1.mode == MecMode::LedgeClimb)
        .count();
    assert_eq!(climbs, 2, "two ledge climbs: {:?}", s.events());
    assert!(s.ps.origin[2] > 299.0, "on the upper top: {:?}", s.ps.origin);
    // Each climb: from the last wallclimb ticks through the ledge climb onto the top.
    let starts: Vec<usize> = (1..s.log.len())
        .filter(|&i| s.log[i - 1].1.mode != MecMode::LedgeClimb && s.log[i].1.mode == MecMode::LedgeClimb)
        .collect();
    let ticks = starts.iter().flat_map(|&a| {
        let b = (a..s.log.len()).find(|&j| s.log[j].1.mode != MecMode::LedgeClimb).unwrap() + 3;
        (a - 3)..=b
    });
    for i in ticks {
        let d = |j: usize| {
            let (p, q) = (s.log[j - 1].0.origin, s.log[j].0.origin);
            [q[0] - p[0], q[1] - p[1], q[2] - p[2]]
        };
        let (d0, d1) = (d(i - 1), d(i));
        // No teleport: a tick's step stays within ~6 in of the one before.
        let jerk = ((d1[0] - d0[0]).powi(2) + (d1[2] - d0[2]).powi(2)).sqrt();
        assert!(jerk < 6.0, "tick {i} ({:?}): step {d1:?} after {d0:?}", s.log[i].1.mode);
        // Never backwards along the climb.
        assert!(d1[0] > -0.05, "tick {i} ({:?}) went back {}", s.log[i].1.mode, d1[0]);
    }
}

// ------------------------------------------------------------ wall turn

/// Two facing walls 3.5 m apart: A (x ≥ 100, 6.5 m, nothing to grab) and B
/// behind, whose lip is 4.75 m up: above the 4.49 m a wallclimb from the
/// ground can heave to, so only wallclimb → quickturn → jump off reaches it.
fn wall_turn_world() -> (BoxWorld, f32, f32) {
    let b_face = 100.0 - m(3.5);
    let lip = m(4.75);
    let world = BoxWorld::floor()
        .with([100.0, -500.0, 0.0], [400.0, 500.0, m(6.5)])
        .with([-400.0, -500.0, 0.0], [b_face, 500.0, lip]);
    (world, b_face, lip)
}

/// Run at A, wallclimb, quickturn after `climb` ticks, press jump `wait`
/// ticks later (forward held throughout).
fn wall_turn(world: &BoxWorld, climb: usize, wait: usize, ticks: usize) -> Sim {
    let mut s = Sim::new([75.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(world, 127, 0);
    s.run(world, climb, 127, buttons::JUMP);
    s.step(world, 127, MecContext::QUICKTURN_BUTTON_DEFAULT);
    s.run(world, wait, 127, 0);
    s.run(world, ticks, 127, buttons::JUMP);
    s
}

fn mode_names(s: &Sim) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for (_, mm, _) in &s.log {
        let n = mm.mode_name();
        if v.last() != Some(&n) {
            v.push(n);
        }
    }
    v
}

#[test]
fn wall_turn_jump_reaches_the_ledge_behind() {
    let (world, b_face, lip) = wall_turn_world();
    let s = wall_turn(&world, 18, 3, 60);
    assert_eq!(
        mode_names(&s),
        [
            "Ground",
            "WallClimb",
            "WallClimb180",
            "Air",
            "LedgeClimb",
            "Ground"
        ],
        "{:?}",
        s.events()
    );
    let ev = s.events();
    let at = |e: MecEvent| ev.iter().position(|x| *x == e).unwrap();
    assert!(at(MecEvent::WallClimbStart) < at(MecEvent::QuickTurn));
    assert!(at(MecEvent::QuickTurn) < at(MecEvent::WallJump));
    assert!(at(MecEvent::WallJump) < at(MecEvent::LedgeClimbStart));
    // WallclimbJump180Driver: 7.2 m/s straight off the wall, 1.1 m jump.
    let jump = s
        .log
        .iter()
        .position(|(_, mm, _)| mm.mode == MecMode::Air)
        .unwrap();
    let (p, _, _) = &s.log[jump];
    near("turn jump speed", -p.velocity[0], m(7.2), 1.0);
    assert!(p.velocity[1].abs() < 10.0, "straight off: {:?}", p.velocity);
    // WallClimb180 held at least until the 20 t jump window.
    let hang = s
        .log
        .iter()
        .filter(|(_, mm, _)| mm.mode_name() == "WallClimb180")
        .count();
    assert!(hang as f32 * DT >= 0.333 - DT, "hang {hang} ticks");
    // On top of B, past its lip.
    assert_eq!(s.mec.mode, MecMode::Ground);
    near("on B's top", s.ps.origin[2], lip, 1.0);
    assert!(s.ps.origin[0] < b_face, "over the lip: {:?}", s.ps.origin);
}

#[test]
fn wall_turn_ledge_needs_the_turn() {
    let (world, b_face, _) = wall_turn_world();
    // Wallclimb straight up B from the ground: its lip is out of reach.
    let mut s = Sim::new([b_face + 25.0, 0.0, 0.125], [0.0; 3], 180.0);
    s.step(&world, 127, 0);
    s.run(&world, 90, 127, buttons::JUMP);
    assert!(s.events().contains(&MecEvent::WallClimbStart));
    assert!(!s.events().contains(&MecEvent::LedgeClimbStart));
    // Climbing A without the turn: no jump off, no ledge.
    let mut s = Sim::new([75.0, 0.0, 0.125], [0.0; 3], 0.0);
    s.step(&world, 127, 0);
    s.run(&world, 18, 127, buttons::JUMP);
    s.run(&world, 3, 127, 0);
    s.run(&world, 80, 127, buttons::JUMP);
    assert!(s.events().contains(&MecEvent::WallClimbStart));
    assert!(!s.events().contains(&MecEvent::WallJump));
    assert!(!s.events().contains(&MecEvent::LedgeClimbStart));
}

#[test]
fn wall_turn_jump_is_buffered_until_its_window() {
    let (world, _, _) = wall_turn_world();
    // Jump pressed on the tick right after the quickturn: held on the wall
    // until 20 t (0.333 s) into the turn, then still reaches B.
    let s = wall_turn(&world, 18, 1, 60);
    let first = s
        .log
        .iter()
        .position(|(_, mm, _)| mm.mode_name() == "WallClimb180")
        .unwrap();
    let jump = s
        .log
        .iter()
        .position(|(_, mm, _)| mm.mode == MecMode::Air)
        .unwrap();
    let held = (jump - first) as f32 * DT;
    assert!((0.333 - DT..0.333 + DT).contains(&held), "held {held} s");
    assert!(s.events().contains(&MecEvent::LedgeClimbStart));
    assert_eq!(s.mec.mode, MecMode::Ground);
}
