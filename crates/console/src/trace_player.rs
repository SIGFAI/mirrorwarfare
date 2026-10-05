//! Opt-in per-tick player trace for external test harnesses (`IW4L_TRACE_PLAYER=1`).
//!
//! One log line per client per presented snapshot tick:
//! `trace: t=<tick> c=<id> L=<lifecycle> o=x,y,z v=x,y,z hp=<n> ang=pitch,yaw g=<ground>
//! m=<mec mode|-> mom=<momentum> pmf=<pm_flags hex> ws=<weaponstate> ads=<frac> clip=<n>
//! k=<kills> d=<deaths> god=<0|1>`. Off by default; reads only the presented snapshot.

use bevy::prelude::*;
use net::PresentedSnapshot;

pub(crate) fn trace_enabled() -> bool {
    std::env::var("IW4L_TRACE_PLAYER").is_ok_and(|v| !v.is_empty() && v != "0")
}

pub(crate) fn trace_players(presented: Option<Res<PresentedSnapshot>>, mut last: Local<Option<u32>>) {
    let Some(presented) = presented else {
        return;
    };
    let Some(snap) = presented.snapshot() else {
        return;
    };
    if *last == Some(snap.tick.0) {
        return;
    }
    *last = Some(snap.tick.0);
    for (id, meta) in &snap.meta.clients {
        let mec = meta
            .mec
            .map(|m| (m.mode_name().to_owned(), m.momentum))
            .unwrap_or_else(|| ("-".into(), 0.0));
        let life = format!("{:?}", meta.lifecycle);
        match snap.players.iter().find(|(c, _)| c == id) {
            Some((_, ps)) => diag::info!(
                Console,
                "trace: t={} c={} L={life} o={:.1},{:.1},{:.1} v={:.0},{:.0},{:.0} hp={} ang={:.1},{:.1} g={} m={} mom={:.2} pmf={:x} ws={} ads={:.2} clip={} k={} d={} god={} vh={:.1}",
                snap.tick.0,
                id.0,
                ps.origin[0],
                ps.origin[1],
                ps.origin[2],
                ps.velocity[0],
                ps.velocity[1],
                ps.velocity[2],
                ps.health,
                ps.viewangles[0],
                ps.viewangles[1],
                ps.ground_entity_num,
                mec.0,
                mec.1,
                ps.pm_flags,
                ps.weaponstate_primary,
                ps.f_weapon_pos_frac,
                meta.ammo_clip,
                meta.kills,
                meta.deaths,
                u8::from(meta.god_mode),
                ps.view_height_current,
            ),
            None => diag::info!(
                Console,
                "trace: t={} c={} L={life} k={} d={}",
                snap.tick.0,
                id.0,
                meta.kills,
                meta.deaths
            ),
        }
    }
}
