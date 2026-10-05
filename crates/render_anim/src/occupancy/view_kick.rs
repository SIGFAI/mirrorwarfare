use asset_game::{WeaponBodyFacts, WeaponKickFacts};
use assets::PreparedWeapons;
use bevy::prelude::*;
use frame::{LifeStarted, ViewSubject};
use hud_iw4::{
    CG_FOV_MIN_DEFAULT, CG_FOV_SCALE_DEFAULT, FovInputs, WeaponAdsOverlayFacts, calc_fov_from_ads,
    horizontal_to_vertical_fov_deg, zoom_sensitivity,
};
use math_iw4::{add_lean_to_position, angle_vectors};
use net::{
    AppliedEntityEventWalk, ClientActionInput, FrameClock, LocalPresentClient, PresentedSnapshot,
};
use playerstate_iw4::ENTITYNUM_NONE;
use weapon_iw4::{
    VIEW_DAMAGE_UNDIRECTED, VIEW_ORG_BOB_Z_MIN_OFS, ViewAngleBobInputs, ViewOrgBobInputs,
    crash_land_fall_height, crash_land_view_dip, damage_feedback_kick, get_viewmodel_weapon_index,
    land_origin_z, view_angle_bob, view_damage_angles, view_org_bob, viewweapon_land_origin_z,
};

use crate::anim::view_kick_state::{KickParams, ViewKickState, add_kick_to_viewangles};
use crate::anim::view_sway::ViewSwayState;
use crate::occupancy::remote_body::RemotePlayer;
use crate::occupancy::third_person::{
    linked_weapon_camera, presented_is_third_person, remote_missile_camera, third_person_camera,
};
use render_scene::{FlyCamera, FpvLens, SimCamera, transform_from_iw_view};
use render_scene::{WorldCameraPose, WorldScriptModelInstance};

const MISSILE_CAM_FOV: f32 = 15.0;
/// A move clip restarts when the server's mode time falls this far behind ours.
const MEC_CLIP_RESTART_MS: i32 = 150;
/// When the move the camera follows changes, its offsets cross-fade from where
/// they were over this long (camera angles / eye offset, and the viewmodel's
/// drop and tilt, which also covers the weapon coming back up) instead of
/// snapping to the new move's values.
const MEC_CAMERA_BLEND_MS: f32 = 200.0;
const MEC_GUN_BLEND_MS: f32 = 300.0;
/// Time constant (s) of the head-bob weight easing in / out with the move.
const MEC_BOB_EASE_S: f32 = 0.08;

/// Client clock for the Catalyst move the camera follows: started from the
/// snapshot's `mode_ms`, advanced by the frame clock so the effects are
/// smooth between snapshots. A roll or hard landing keeps playing to the end
/// of its clip after control has returned.
#[derive(Clone, Copy, Debug, Default)]
pub struct MecClipTrack {
    live: bool,
    mode: u8,
    kind: i8,
    move_ms: i32,
    start_ms: i32,
    /// Offsets when the current clip took over, faded out from `blend_ms`.
    from: movement_mec::MecCameraMotion,
    blend_ms: i32,
    /// Last offsets returned (the cross-fade source on the next change).
    last: movement_mec::MecCameraMotion,
}

fn wrap_deg(a: f32) -> f32 {
    let a = a % 360.0;
    if a > 180.0 {
        a - 360.0
    } else if a < -180.0 {
        a + 360.0
    } else {
        a
    }
}

fn smooth01(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// `from` faded into `to`: camera terms by `cam`, viewmodel terms by `gun`
/// (0 = all `from`, 1 = all `to`).
fn mix_motion(
    from: movement_mec::MecCameraMotion,
    to: movement_mec::MecCameraMotion,
    cam: f32,
    gun: f32,
) -> movement_mec::MecCameraMotion {
    let l = |a: f32, b: f32, k: f32| a + (b - a) * k;
    movement_mec::MecCameraMotion {
        pitch: l(from.pitch, to.pitch, cam),
        roll: l(from.roll, to.roll, cam),
        z: l(from.z, to.z, cam),
        gun_drop: l(from.gun_drop, to.gun_drop, gun),
        gun_back: l(from.gun_back, to.gun_back, gun),
        gun_pitch: l(from.gun_pitch, to.gun_pitch, gun),
        gun_yaw: l(from.gun_yaw, to.gun_yaw, gun),
        gun_roll: l(from.gun_roll, to.gun_roll, gun),
        body_roll: l(from.body_roll, to.body_roll, cam),
        body_shift: l(from.body_shift, to.body_shift, cam),
    }
}

fn mec_camera_motion(
    track: &mut MecClipTrack,
    mec: Option<&sim::MecMoveState>,
    now: i32,
) -> movement_mec::MecCameraMotion {
    let tuning = movement_mec::MecTuning::DEFAULT;
    let Some(mec) = mec else {
        *track = MecClipTrack::default();
        return movement_mec::MecCameraMotion::default();
    };
    let elapsed_ms = now.wrapping_sub(track.start_ms);
    let mode = mec.mode as u8;
    let changed = !track.live
        || track.mode != mode
        || track.kind != mec.wall_side
        || mec.mode_ms + MEC_CLIP_RESTART_MS < elapsed_ms;
    if changed {
        let old = sim::MecMode::from_u8(track.mode);
        let lingering = track.live
            && matches!(mec.mode, sim::MecMode::Ground)
            && matches!(old, sim::MecMode::Roll | sim::MecMode::HardLanding)
            && {
                let probe = sim::MecMoveState {
                    mode: old,
                    wall_side: track.kind,
                    ..*mec
                };
                (elapsed_ms as f32 / 1000.0) < probe.camera_clip(0.0, &tuning).duration
            };
        if !lingering {
            // Angles are cross-faded the short way round (a finished roll
            // sits at 360° of pitch, which is level).
            let mut from = track.last;
            for a in [
                &mut from.pitch,
                &mut from.roll,
                &mut from.gun_pitch,
                &mut from.gun_yaw,
                &mut from.gun_roll,
                &mut from.body_roll,
            ] {
                *a = wrap_deg(*a);
            }
            *track = MecClipTrack {
                live: true,
                mode,
                kind: mec.wall_side,
                move_ms: mec.move_ms,
                start_ms: now.wrapping_sub(mec.mode_ms),
                from: if track.live {
                    from
                } else {
                    movement_mec::MecCameraMotion::default()
                },
                blend_ms: now,
                last: track.last,
            };
        }
    }
    let elapsed = now.wrapping_sub(track.start_ms).max(0) as f32 / 1000.0;
    let clip_state = sim::MecMoveState {
        mode: sim::MecMode::from_u8(track.mode),
        wall_side: track.kind,
        move_ms: track.move_ms,
        ..*mec
    };
    let target = movement_mec::camera_motion(clip_state.camera_clip(elapsed, &tuning), &tuning);
    let since = now.wrapping_sub(track.blend_ms).max(0) as f32;
    let motion = mix_motion(
        track.from,
        target,
        smooth01(since / MEC_CAMERA_BLEND_MS),
        smooth01(since / MEC_GUN_BLEND_MS),
    );
    track.last = motion;
    motion
}

fn kick_params(k: &WeaponKickFacts) -> KickParams {
    KickParams {
        f_ads_view_kick_center_speed: k.f_ads_view_kick_center_speed,
        f_hip_view_kick_center_speed: k.f_hip_view_kick_center_speed,
        gun_max_pitch: k.gun_max_pitch,
        gun_max_yaw: k.gun_max_yaw,
        ads_gun_kick_reduced_kick_percent: k.ads_gun_kick_reduced_kick_percent,
        ads_gun_kick_pitch_min: k.ads_gun_kick_pitch_min,
        ads_gun_kick_pitch_max: k.ads_gun_kick_pitch_max,
        ads_gun_kick_yaw_min: k.ads_gun_kick_yaw_min,
        ads_gun_kick_yaw_max: k.ads_gun_kick_yaw_max,
        ads_gun_kick_accel: k.ads_gun_kick_accel,
        ads_gun_kick_speed_max: k.ads_gun_kick_speed_max,
        ads_gun_kick_speed_decay: k.ads_gun_kick_speed_decay,
        ads_gun_kick_static_decay: k.ads_gun_kick_static_decay,
        ads_view_kick_pitch_min: k.ads_view_kick_pitch_min,
        ads_view_kick_pitch_max: k.ads_view_kick_pitch_max,
        ads_view_kick_yaw_min: k.ads_view_kick_yaw_min,
        ads_view_kick_yaw_max: k.ads_view_kick_yaw_max,
        hip_gun_kick_reduced_kick_percent: k.hip_gun_kick_reduced_kick_percent,
        hip_gun_kick_pitch_min: k.hip_gun_kick_pitch_min,
        hip_gun_kick_pitch_max: k.hip_gun_kick_pitch_max,
        hip_gun_kick_yaw_min: k.hip_gun_kick_yaw_min,
        hip_gun_kick_yaw_max: k.hip_gun_kick_yaw_max,
        hip_gun_kick_accel: k.hip_gun_kick_accel,
        hip_gun_kick_speed_max: k.hip_gun_kick_speed_max,
        hip_gun_kick_speed_decay: k.hip_gun_kick_speed_decay,
        hip_gun_kick_static_decay: k.hip_gun_kick_static_decay,
        hip_view_kick_pitch_min: k.hip_view_kick_pitch_min,
        hip_view_kick_pitch_max: k.hip_view_kick_pitch_max,
        hip_view_kick_yaw_min: k.hip_view_kick_yaw_min,
        hip_view_kick_yaw_max: k.hip_view_kick_yaw_max,
    }
}

#[derive(Resource, Default)]
pub struct PendingViewHurt(pub u32);

#[derive(Resource, Default)]
pub struct SessionViewKick {
    pub state: ViewKickState,
    pub sway: ViewSwayState,

    pub placement_move_origin: [f32; 3],

    pub placement_move_angles: [f32; 3],

    pub weap_idle_time: i32,

    pub last_idle_factor: f32,

    pub view_last_idle_factor: f32,

    pub land_change: f32,

    pub land_time: i32,

    pub land_view_dip: i32,

    pub viewweapon_land_z: f32,

    pub viewweapon_land_view: [f32; 3],

    pub refdef_view_angles: [f32; 3],

    pub refdef_vieworg: [f32; 3],

    pub horiz_fov_deg: f32,

    pub killcam_focus_distance: Option<f32>,

    pub damage_time: i32,

    pub v_dmg_pitch: f32,

    pub v_dmg_roll: f32,
    last_weapon_id: u32,
    last_origin: [f32; 3],
    last_velocity: [f32; 3],
    last_ground_entity: i32,
    have_land_prev: bool,
    last_damage_event: u32,
    have_damage_prev: bool,

    pub seeded_this_frame: u32,

    pub(super) last_weapon_pos_frac: f32,

    pub b_position_to_ads: bool,

    pub mec_roll: f32,

    pub mec_track: MecClipTrack,

    /// Catalyst move camera / viewmodel offsets this frame.
    pub mec_motion: movement_mec::MecCameraMotion,

    /// Catalyst movement: weight (0..=1) of the IW4 head bob, eased toward 1
    /// on the ground and 0 in every other move (no footsteps there, and the
    /// sprint-lowered weapon flag would otherwise double the bob amplitude in
    /// one frame at a vault / climb / roll start and end).
    pub mec_bob: f32,
    /// Catalyst mode on the previous frame (landing-dip gating).
    pub mec_last_mode: Option<u8>,
}

impl SessionViewKick {
    pub fn clear_for_new_life(&mut self) {
        self.state.reset();
        self.sway.reset();
        self.placement_move_origin = [0.0; 3];
        self.placement_move_angles = [0.0; 3];
        self.weap_idle_time = 0;
        self.last_idle_factor = 0.0;
        self.view_last_idle_factor = 0.0;
        self.land_change = 0.0;
        self.land_time = 0;
        self.land_view_dip = 0;
        self.viewweapon_land_z = 0.0;
        self.viewweapon_land_view = [0.0; 3];
        self.damage_time = 0;
        self.v_dmg_pitch = 0.0;
        self.v_dmg_roll = 0.0;
        self.last_origin = [0.0; 3];
        self.last_velocity = [0.0; 3];
        self.last_ground_entity = 0;
        self.have_land_prev = false;
        self.last_damage_event = 0;
        self.have_damage_prev = false;
        self.seeded_this_frame = 0;
        self.last_weapon_pos_frac = 0.0;
        self.b_position_to_ads = true;
        self.mec_roll = 0.0;
        self.mec_track = MecClipTrack::default();
        self.mec_motion = movement_mec::MecCameraMotion::default();
        self.mec_bob = 1.0;
        self.mec_last_mode = None;
    }
}

pub fn reset_view_kick_on_life_started(
    local: Res<LocalPresentClient>,
    mut started: MessageReader<LifeStarted>,
    mut kick: ResMut<SessionViewKick>,
    mut hurt: ResMut<PendingViewHurt>,
) {
    for ev in started.read() {
        if ev.client == local.0.0 {
            kick.clear_for_new_life();
            hurt.0 = 0;
        }
    }
}

pub fn tick_session_view_kick(
    clock: Res<FrameClock>,
    presented: Res<PresentedSnapshot>,
    local: Res<LocalPresentClient>,
    weapons: Option<Res<PreparedWeapons>>,
    walk: Option<Res<AppliedEntityEventWalk>>,
    mut kick: ResMut<SessionViewKick>,
) {
    let Some(ps) = presented.player(local.0) else {
        return;
    };
    let viewmodel = get_viewmodel_weapon_index(ps);
    if viewmodel != kick.last_weapon_id {
        kick.state.reset();
        kick.sway.reset();
        kick.placement_move_origin = [0.0; 3];
        kick.placement_move_angles = [0.0; 3];
        kick.weap_idle_time = 0;
        kick.last_idle_factor = 0.0;
        kick.view_last_idle_factor = 0.0;
        kick.seeded_this_frame = 0;
        kick.last_weapon_id = viewmodel;
        kick.last_weapon_pos_frac = 0.0;
        kick.b_position_to_ads = true;
    }

    let Some(reg) = weapons.as_ref() else {
        return;
    };
    let Some(facts) = reg.0.facts_of(viewmodel) else {
        return;
    };
    if !facts.body_resolved {
        return;
    }

    let dt = clock.frametime_secs();
    kick.seeded_this_frame = 0;

    let n = walk.map(|w| w.local_fire).unwrap_or(0);
    if n > 0 {
        let reduce_window_active = ps.weapon_restrict_kick_time > 0;
        let params = kick_params(&facts.kick);
        for _ in 0..n {
            kick.state.seed_fire(
                &params,
                ps.f_weapon_pos_frac,
                ps.weap_flags,
                ps.recoil_scale,
                reduce_window_active,
            );
        }
        kick.seeded_this_frame = n;
    }

    let dt_ms = clock.frametime();
    let weapon_index = if viewmodel == 0 { 0 } else { 1 };
    let params = kick_params(&facts.kick);
    kick.state
        .advance(&params, weapon_index, ps.f_weapon_pos_frac, dt_ms);

    let overlay_active = facts.overlay_reticle != 0;
    kick.sway.advance(
        facts.sway.hip_params(),
        facts.sway.ads_params(),
        ps.viewangles,
        ps.f_weapon_pos_frac,
        facts.aim_down_sight,
        overlay_active,
        1.0,
        dt,
    );
}

pub fn sync_camera_from_presented(
    mut killcam: Local<super::killcam::KillcamCamera>,
    clock: Res<FrameClock>,
    presented: Res<PresentedSnapshot>,
    local: Res<LocalPresentClient>,
    sim_cam: Res<SimCamera>,
    settings: Res<frame::GameSettings>,
    mut kick: ResMut<SessionViewKick>,
    mut hurt: ResMut<PendingViewHurt>,
    weapons: Option<Res<PreparedWeapons>>,
    mut actions: Option<ResMut<ClientActionInput>>,
    mut q: Query<
        &mut Transform,
        (
            With<FlyCamera>,
            Without<RemotePlayer>,
            Without<WorldScriptModelInstance>,
        ),
    >,
    mut lenses: Query<&mut Projection, With<FpvLens>>,
    view: Res<ViewSubject>,
    death_cam_clip: Res<crate::occupancy::dyn_ent::DynEntPhysClip>,
) {
    kick.killcam_focus_distance = None;
    if !sim_cam.enabled {
        return;
    }
    let Some(ps) = presented.player(local.0) else {
        return;
    };
    let viewmodel = get_viewmodel_weapon_index(ps);
    if let Some((pose, fov, focus_distance)) = killcam.update(
        &presented,
        local.0,
        clock.time(),
        view.in_killcam(),
        weapons.as_deref(),
        death_cam_clip.0.as_deref(),
    ) {
        let pose = earthquake_pose(pose, &presented, clock.time());
        let eye = transform_from_iw_view(pose);
        for mut transform in &mut q {
            transform.translation = eye.translation;
            transform.rotation = eye.rotation;
        }
        for mut projection in &mut lenses {
            if let Projection::Perspective(perspective) = &mut *projection {
                perspective.fov = horizontal_to_vertical_fov_deg(fov).to_radians();
            }
        }
        kick.horiz_fov_deg = fov;
        kick.refdef_vieworg = pose.origin;
        kick.refdef_view_angles = pose.angles;
        kick.killcam_focus_distance = focus_distance;
        return;
    }
    if let Some(pose) = remote_missile_camera(&presented, local.0, clock.time()) {
        let pose = earthquake_pose(pose, &presented, clock.time());
        let eye = transform_from_iw_view(pose);
        for mut transform in &mut q {
            transform.translation = eye.translation;
            transform.rotation = eye.rotation;
        }
        let horiz = MISSILE_CAM_FOV;
        if let Some(actions) = actions.as_deref_mut() {
            actions.fov_scale = zoom_sensitivity(horiz) * actions.shellshock_look_scale;
        }
        let vertical = horizontal_to_vertical_fov_deg(horiz).to_radians();
        for mut projection in lenses.iter_mut() {
            if let Projection::Perspective(perspective) = &mut *projection {
                perspective.fov = vertical;
            }
        }
        kick.horiz_fov_deg = horiz;
        return;
    }
    if presented_is_third_person(
        &presented,
        local.0,
        view.in_killcam(),
        settings.third_person,
    ) {
        let Some(pose) = linked_weapon_camera(&presented, local.0)
            .or_else(|| third_person_camera(&presented, local.0, death_cam_clip.0.as_deref()))
        else {
            return;
        };
        let pose = earthquake_pose(pose, &presented, clock.time());
        let eye = transform_from_iw_view(pose);
        for mut transform in &mut q {
            transform.translation = eye.translation;
            transform.rotation = eye.rotation;
        }
        kick.refdef_vieworg = pose.origin;
        kick.refdef_view_angles = pose.angles;
        kick.horiz_fov_deg = apply_fpv_lens_fov(
            &mut lenses,
            settings.fov,
            ps.pm_type,
            ps.link_flags,
            ps.e_flags,
            0.0,
            viewmodel,
            weapons.as_ref().and_then(|w| w.0.facts_of(viewmodel)),
            false,
            actions.as_deref_mut(),
        )
        .unwrap_or(settings.fov);
        return;
    }
    let offset = presented.view_offset();
    let xyspeed = {
        let vx = ps.velocity[0];
        let vy = ps.velocity[1];
        math_iw4::vec3_length([vx, vy, 0.0])
    };
    let mec = presented
        .snapshot()
        .and_then(|s| s.meta.for_client(local.0))
        .and_then(|meta| meta.mec.as_ref());
    // Catalyst movement lowers the weapon through the IW4 sprint flag; that is
    // not a sprint, so it must not switch the head bob to sprint amplitude.
    let bob_pm_flags = if mec.is_some() {
        ps.pm_flags & !playerstate_iw4::pm_flags::SPRINTING
    } else {
        ps.pm_flags
    };
    let org = ViewOrgBobInputs {
        bob_cycle: (ps.bob_cycle as u32 & 0xff) as u8,
        xyspeed,
        view_height_target: ps.view_height_target,
        pm_flags: bob_pm_flags,
        weapon_pos_frac: ps.f_weapon_pos_frac,
        perks0: ps.perks[0],
    };
    stamp_damage_feedback(
        &mut kick,
        &mut hurt,
        ps.damage_event,
        ps.damage_yaw,
        ps.damage_pitch,
        ps.damage_count,
        ps.viewangles,
        clock.time(),
    );
    let bob_angles = match weapons.as_ref().and_then(|w| w.0.facts_of(viewmodel)) {
        Some(facts) if facts.body_resolved => view_angle_bob(ViewAngleBobInputs {
            org,
            e_flags: ps.e_flags,
            overlay_reticle: facts.overlay_reticle,
            ads_bob_factor: facts.ads_bob_factor,
            ads_view_bob_mult: facts.ads_view_bob_mult,
            time: clock.time(),
            damage_time: kick.damage_time,
            v_dmg_pitch: kick.v_dmg_pitch,
            v_dmg_roll: kick.v_dmg_roll,
            aim_down_sight: facts.aim_down_sight,
            idle: facts.idle,
            frametime: clock.frametime_secs(),
            hold_breath_scale: ps.hold_breath_scale,
            weap_idle_time: kick.weap_idle_time,
            view_last_idle_factor: kick.view_last_idle_factor,
        }),
        _ => view_damage_angles(ViewAngleBobInputs {
            org,
            time: clock.time(),
            damage_time: kick.damage_time,
            v_dmg_pitch: kick.v_dmg_pitch,
            v_dmg_roll: kick.v_dmg_roll,
            ..ViewAngleBobInputs::default()
        }),
    };
    kick.weap_idle_time = bob_angles.weap_idle_time;
    kick.view_last_idle_factor = bob_angles.view_last_idle_factor;
    let kick_angles = add_kick_to_viewangles(ps.viewangles, kick.state.kick_angles);
    let motion = mec_camera_motion(&mut kick.mec_track, mec, clock.time());
    kick.mec_motion = motion;
    kick.mec_roll = motion.roll;
    let angles = [
        kick_angles[0] + bob_angles.pitch + motion.pitch,
        kick_angles[1] + bob_angles.yaw,
        kick_angles[2] + bob_angles.roll + kick.mec_roll,
    ];
    kick.refdef_view_angles = angles;
    let mut origin = [
        ps.origin[0] + offset[0],
        ps.origin[1] + offset[1],
        ps.origin[2] + offset[2] + ps.view_height_current,
    ];
    let mut bob = view_org_bob(org);
    if let Some(m) = mec {
        let on_feet = matches!(m.mode, sim::MecMode::Ground | sim::MecMode::Slide);
        let k = 1.0 - (-clock.frametime_secs().max(0.0) / MEC_BOB_EASE_S).exp();
        kick.mec_bob += (f32::from(u8::from(on_feet)) - kick.mec_bob) * k;
        bob.vertical *= kick.mec_bob;
        bob.horizontal *= kick.mec_bob;
    }
    origin[2] += bob.vertical;

    let (fwd, right, up) = angle_vectors(angles);
    origin[0] += bob.horizontal * right[0];
    origin[1] += bob.horizontal * right[1];
    origin[2] += bob.horizontal * right[2];
    // The IW4 landing dip is for falls: a Catalyst roll / hard landing has its
    // own camera dip, and a vault or ledge climb hands back to the ground from
    // a scripted path (no ground entity while on it), not from a fall.
    let mode_now = mec.map(|m| m.mode as u8);
    let allow_dip = match (kick.mec_last_mode, mode_now) {
        (_, None) => true,
        (Some(prev), Some(now)) => {
            prev == sim::MecMode::Air as u8 && now == sim::MecMode::Ground as u8
                || prev == sim::MecMode::Air as u8 && now == sim::MecMode::Slide as u8
        }
        (None, Some(_)) => false,
    };
    kick.mec_last_mode = mode_now;
    let land_ofs = stamp_and_land_origin_z(
        &mut kick,
        ps.gravity,
        ps.origin,
        ps.velocity,
        ps.ground_entity_num,
        clock.time(),
        allow_dip,
    );
    origin[2] += land_ofs + motion.z;
    let delta_ms = clock.time().wrapping_sub(kick.land_time);
    kick.viewweapon_land_z = viewweapon_land_origin_z(delta_ms, kick.land_change);
    kick.viewweapon_land_view = [
        kick.viewweapon_land_z * fwd[2],
        kick.viewweapon_land_z * right[2],
        kick.viewweapon_land_z * up[2],
    ];
    origin = add_lean_to_position(origin, ps.viewangles[1], ps.leanf, 16.0, 20.0);
    let min_z = ps.origin[2] + offset[2] + VIEW_ORG_BOB_Z_MIN_OFS;
    if origin[2] < min_z {
        origin[2] = min_z;
    }
    let pose = earthquake_pose(WorldCameraPose { origin, angles }, &presented, clock.time());
    kick.refdef_vieworg = pose.origin;
    kick.refdef_view_angles = pose.angles;
    let eye = transform_from_iw_view(pose);
    for mut transform in &mut q {
        transform.translation = eye.translation;
        transform.rotation = eye.rotation;
    }
    if camera_trace_enabled() {
        // One line per rendered frame (`IW4L_TRACE_PLAYER=1`): what the
        // first-person camera did, for frame-to-frame continuity checks.
        diag::info!(
            Fpv,
            "camtrace: ms={} tick={} fi={:.3} eye={:.2},{:.2},{:.2} ang={:.2},{:.2},{:.2} vh={:.2} mp={:.2} mz={:.2} gd={:.2} gp={:.2} ws={} mode={} pz={:.2} bob={:.2} land={:.2}",
            clock.time(),
            presented.tick().map_or(0, |t| t.0),
            presented.interpolation_pair().map_or(-1.0, |(_, _, f)| f),
            pose.origin[0],
            pose.origin[1],
            pose.origin[2],
            pose.angles[0],
            pose.angles[1],
            pose.angles[2],
            ps.view_height_current,
            motion.pitch,
            motion.z,
            motion.gun_drop,
            motion.gun_pitch,
            ps.weaponstate_primary,
            mec.map_or(-1, |m| m.mode as i32),
            ps.origin[2],
            bob.vertical,
            land_ofs,
        );
    }

    if ps.f_weapon_pos_frac > kick.last_weapon_pos_frac {
        kick.b_position_to_ads = true;
    } else if ps.f_weapon_pos_frac < kick.last_weapon_pos_frac {
        kick.b_position_to_ads = false;
    }
    kick.last_weapon_pos_frac = ps.f_weapon_pos_frac;
    if let Some(horiz) = apply_fpv_lens_fov(
        &mut lenses,
        settings.fov,
        ps.pm_type,
        ps.link_flags,
        ps.e_flags,
        ps.f_weapon_pos_frac,
        viewmodel,
        weapons.as_ref().and_then(|w| w.0.facts_of(viewmodel)),
        kick.b_position_to_ads,
        actions.as_deref_mut(),
    ) {
        kick.horiz_fov_deg = horiz;
    }
}

fn apply_fpv_lens_fov(
    lenses: &mut Query<&mut Projection, With<FpvLens>>,
    base_fov: f32,
    pm_type: i32,
    link_flags: u32,
    e_flags: u32,
    f_weapon_pos_frac: f32,
    viewmodel: u32,
    facts: Option<WeaponBodyFacts>,
    b_position_to_ads: bool,
    actions: Option<&mut ClientActionInput>,
) -> Option<f32> {
    let facts = facts.filter(|f| f.body_resolved).unwrap_or_default();
    let overlay = WeaponAdsOverlayFacts {
        ads_zoom_in_frac: facts.ads_zoom_in_frac,
        ads_zoom_out_frac: facts.ads_zoom_out_frac,
        overlay_reticle: facts.overlay_reticle,
        ..WeaponAdsOverlayFacts::default()
    };
    let ads_target = if facts.ads_zoom_fov > 0.0 {
        facts.ads_zoom_fov
    } else {
        base_fov
    };
    let inputs = FovInputs {
        cg_fov: base_fov,
        pm_type,
        link_flags,
        e_flags,
        weapon_index_nonzero: viewmodel != 0,
        aim_down_sight: facts.aim_down_sight && facts.ads_zoom_fov > 0.0,
        ads_zoom_fov: ads_target,
        overlay_zoom: 0.0,
        fov_scale: CG_FOV_SCALE_DEFAULT,
        fov_min: CG_FOV_MIN_DEFAULT,
    };
    let (horiz, _) = calc_fov_from_ads(&inputs, f_weapon_pos_frac, b_position_to_ads, &overlay);
    let zoom_sensitivity = zoom_sensitivity(horiz);
    if let Some(actions) = actions {
        actions.fov_scale = zoom_sensitivity * actions.shellshock_look_scale;
    }
    let vertical = horizontal_to_vertical_fov_deg(horiz).to_radians();
    for mut projection in lenses.iter_mut() {
        if let Projection::Perspective(perspective) = &mut *projection {
            perspective.fov = vertical;
        }
    }
    Some(horiz)
}

fn stamp_and_land_origin_z(
    kick: &mut SessionViewKick,
    gravity: i32,
    origin: [f32; 3],
    velocity: [f32; 3],
    ground_entity: i32,
    cg_time: i32,
    allow_dip: bool,
) -> f32 {
    if allow_dip
        && kick.have_land_prev
        && kick.last_ground_entity == ENTITYNUM_NONE
        && ground_entity != ENTITYNUM_NONE
    {
        if let Some(fall) = crash_land_fall_height(
            gravity,
            kick.last_origin[2],
            origin[2],
            kick.last_velocity[2],
        ) {
            let dip = crash_land_view_dip(fall);
            if dip > 0 {
                kick.land_change = -(dip as f32);
                kick.land_time = cg_time;
                kick.land_view_dip = dip;
            }
        }
    }
    kick.last_origin = origin;
    kick.last_velocity = velocity;
    kick.last_ground_entity = ground_entity;
    kick.have_land_prev = true;
    let delta = cg_time.wrapping_sub(kick.land_time) as f32;
    land_origin_z(delta, kick.land_change)
}

fn stamp_damage_feedback(
    kick: &mut SessionViewKick,
    hurt: &mut PendingViewHurt,
    damage_event: u32,
    damage_yaw: u32,
    damage_pitch: u32,
    damage_count: i32,
    viewangles: [f32; 3],
    cg_time: i32,
) {
    let mut stamped = false;
    if kick.have_damage_prev && damage_event != kick.last_damage_event && damage_count != 0 {
        let punch = damage_feedback_kick(damage_yaw, damage_pitch, damage_count, viewangles);
        kick.v_dmg_pitch = punch.v_dmg_pitch;
        kick.v_dmg_roll = punch.v_dmg_roll;
        kick.damage_time = cg_time.max(1);
        stamped = true;
    }
    if !stamped && hurt.0 > 0 {
        hurt.0 -= 1;
        let punch = damage_feedback_kick(
            VIEW_DAMAGE_UNDIRECTED,
            VIEW_DAMAGE_UNDIRECTED,
            1,
            viewangles,
        );
        kick.v_dmg_pitch = punch.v_dmg_pitch;
        kick.v_dmg_roll = punch.v_dmg_roll;
        kick.damage_time = cg_time.max(1);
    }
    kick.last_damage_event = damage_event;
    kick.have_damage_prev = true;
}

#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct GunOffset {
    pub x: f32,

    pub y: f32,

    pub z: f32,
}

impl GunOffset {
    pub fn xyz(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

pub(crate) fn apply_cg_gun_offset_view(origin: [f32; 3], gun: [f32; 3]) -> [f32; 3] {
    [origin[0] + gun[0], origin[1] + gun[1], origin[2] + gun[2]]
}

pub(crate) fn apply_viewweapon_land_view(origin: [f32; 3], land_view: [f32; 3]) -> [f32; 3] {
    [
        origin[0] + land_view[0],
        origin[1] + land_view[1],
        origin[2] + land_view[2],
    ]
}

pub(crate) fn iw_view_placement_to_bevy_camera_local(
    origin: [f32; 3],
    angles_deg: [f32; 3],
) -> Transform {
    let [fwd, right, up] = origin;
    let translation = Vec3::new(right, up, -fwd);
    Transform {
        translation,
        rotation: crate::anim::fpv_pose::placement_angles_to_bevy_camera_quat(angles_deg),
        ..Default::default()
    }
}

fn earthquake_pose(
    mut pose: WorldCameraPose,
    presented: &PresentedSnapshot,
    now_ms: i32,
) -> WorldCameraPose {
    if let Some(snapshot) = presented.snapshot() {
        let eye = pose.origin;
        for quake in &snapshot.meta.objectives.earthquakes {
            let offset = quake.angle_offset(eye, now_ms);
            for (angle, delta) in pose.angles.iter_mut().zip(offset) {
                *angle += delta;
            }
        }
    }
    pose
}

#[cfg(test)]
mod mec_camera_tests {
    use super::*;

    /// Frames of `ms` each through a list of (mode, kind, move_ms, frames)
    /// segments; returns every frame's offsets.
    fn play(segments: &[(sim::MecMode, i8, i32, i32)], ms: i32) -> Vec<movement_mec::MecCameraMotion> {
        let mut track = MecClipTrack::default();
        let mut now = 10_000;
        let mut out = Vec::new();
        for &(mode, kind, move_ms, frames) in segments {
            for f in 0..frames {
                let mec = sim::MecMoveState {
                    mode,
                    wall_side: kind,
                    move_ms,
                    mode_ms: f * ms,
                    ..sim::MecMoveState::SPAWN
                };
                out.push(mec_camera_motion(&mut track, Some(&mec), now));
                now += ms;
            }
        }
        out
    }

    fn max_step(frames: &[movement_mec::MecCameraMotion], f: impl Fn(&movement_mec::MecCameraMotion) -> f32) -> f32 {
        frames.windows(2).map(|w| (f(&w[1]) - f(&w[0])).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn move_changes_cross_fade_instead_of_snapping() {
        use sim::MecMode as M;
        let k = movement_mec::script_kind::LEDGE_HIGH;
        // Wallclimb cut short by a ledge grab, the climb, then running on.
        let frames = play(&[(M::WallClimb, 0, 1000, 30), (M::LedgeClimb, k, 1000, 62), (M::Ground, 0, 0, 30)], 16);
        assert!(max_step(&frames, |m| m.pitch) < 1.5, "pitch step {}", max_step(&frames, |m| m.pitch));
        assert!(max_step(&frames, |m| m.gun_drop) < 1.0, "gun drop step {}", max_step(&frames, |m| m.gun_drop));
        assert!(max_step(&frames, |m| m.gun_pitch) < 3.0);
        // A wallrun left early (jump off): the 6° roll eases out.
        let frames = play(&[(M::WallRun, -1, 1333, 30), (M::Air, 0, 0, 30)], 16);
        assert!(max_step(&frames, |m| m.roll) < 1.0, "roll step {}", max_step(&frames, |m| m.roll));
        // A roll ends level (360° of pitch) and is not unwound backwards.
        let frames = play(&[(M::Roll, 0, 1000, 63), (M::Ground, 0, 0, 40)], 16);
        let after: Vec<f32> = frames[74..].iter().map(|m| m.pitch).collect();
        assert!(after.iter().all(|p| p.abs() < 2.0 || (p - 360.0).abs() < 2.0), "{after:?}");
    }
}

/// `IW4L_TRACE_PLAYER=1`: log the first-person camera every frame.
fn camera_trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("IW4L_TRACE_PLAYER").is_ok_and(|v| !v.is_empty() && v != "0"))
}
