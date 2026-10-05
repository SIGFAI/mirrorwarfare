use std::fmt::Write as _;

use asset_world::{IntermissionView, MinimapCorners, SpawnPoint};

use crate::ArenaPackage;

/// Every arena spawn serves each mode family: free-for-all, both team
/// sides, and the first-spawn variants the start logic looks for.
const SPAWN_CLASSES: &[&str] = &[
    "mp_dm_spawn",
    "mp_tdm_spawn",
    "mp_tdm_spawn_allies_start",
    "mp_tdm_spawn_axis_start",
];

pub fn spawn_points(arena: &ArenaPackage) -> Vec<SpawnPoint> {
    arena
        .spawns
        .iter()
        .map(|spawn| SpawnPoint {
            classname: "mp_dm_spawn".to_owned(),
            origin: spawn.origin,
            angles: [0.0, spawn.yaw, 0.0],
            script_linkto: String::new(),
            script_destructable_area: String::new(),
        })
        .collect()
}

fn vec3(v: [f32; 3]) -> String {
    format!("{} {} {}", v[0], v[1], v[2])
}

/// The map entity string a Radiant compile would have produced for this
/// arena: worldspawn, spawns, an intermission camera and minimap corners.
pub fn entity_string(arena: &ArenaPackage) -> String {
    let mut out = String::new();
    let mut block = |pairs: &[(&str, String)]| {
        out.push_str("{\n");
        for (key, value) in pairs {
            let _ = writeln!(out, "\"{key}\" \"{value}\"");
        }
        out.push_str("}\n");
    };
    block(&[
        ("classname", "worldspawn".to_owned()),
        ("northyaw", "90".to_owned()),
        ("ambient", ".15".to_owned()),
    ]);
    for spawn in &arena.spawns {
        for class in SPAWN_CLASSES {
            block(&[
                ("classname", (*class).to_owned()),
                ("origin", vec3(spawn.origin)),
                ("angles", vec3([0.0, spawn.yaw, 0.0])),
            ]);
        }
    }
    // `_airdrop::init` reads its crate template's collision from these two.
    let below = vec3([0.0, 0.0, arena.mins[2] - 4096.0]);
    block(&[
        ("classname", "script_brushmodel".to_owned()),
        ("targetname", "care_package".to_owned()),
        ("target", "care_package_collision".to_owned()),
        ("origin", below.clone()),
    ]);
    block(&[
        ("classname", "script_brushmodel".to_owned()),
        ("targetname", "care_package_collision".to_owned()),
        ("origin", below),
    ]);
    let view = intermission_view(arena);
    block(&[
        ("classname", "mp_global_intermission".to_owned()),
        ("origin", vec3(view.origin)),
        ("angles", vec3(view.angles)),
    ]);
    let corners = minimap_corners(arena);
    for corner in [corners.a, corners.b] {
        let corner = [corner[0], corner[1], (arena.mins[2] + arena.maxs[2]) * 0.5];
        block(&[
            ("classname", "script_origin".to_owned()),
            ("targetname", "minimap_corner".to_owned()),
            ("origin", vec3(corner)),
        ]);
    }
    out
}

/// Above a corner of the bounds, looking down at the centre.
pub fn intermission_view(arena: &ArenaPackage) -> IntermissionView {
    let centre = [0, 1, 2].map(|a| (arena.mins[a] + arena.maxs[a]) * 0.5);
    let span = (arena.maxs[0] - arena.mins[0]).max(arena.maxs[1] - arena.mins[1]);
    let eye = [
        arena.mins[0] + span * 0.05,
        arena.mins[1] + span * 0.05,
        arena.maxs[2] + span * 0.25,
    ];
    let to = [centre[0] - eye[0], centre[1] - eye[1], centre[2] - eye[2]];
    let yaw = to[1].atan2(to[0]).to_degrees();
    let pitch = -(to[2].atan2(to[0].hypot(to[1]))).to_degrees();
    IntermissionView {
        origin: eye,
        angles: [pitch, yaw, 0.0],
    }
}

/// Top-left and bottom-right of the bounds, as Radiant's `minimap_corner` pair.
pub fn minimap_corners(arena: &ArenaPackage) -> MinimapCorners {
    MinimapCorners {
        a: [arena.mins[0], arena.maxs[1]],
        b: [arena.maxs[0], arena.mins[1]],
    }
}

/// `maps/mp/<map>.gsc`: what every stock map main does that an arena needs,
/// plus the arena's out-of-bounds rule: below the floor or outside the cut
/// square (with a little slack for edge parkour) the map kills you.
pub fn map_script(arena: &ArenaPackage) -> String {
    const SLACK: f32 = 64.0;
    let (lo, hi) = (arena.play_mins, arena.play_maxs);
    let lo = format!("( {}, {}, {} )", lo[0] - SLACK, lo[1] - SLACK, lo[2]);
    let hi = format!("( {}, {}, {} )", hi[0] + SLACK, hi[1] + SLACK, hi[2]);
    r#"main()
{
	maps\mp\_load::main();

	game[ "attackers" ] = "allies";
	game[ "defenders" ] = "axis";

	level thread out_of_bounds( LO, HI );
}

out_of_bounds( lo, hi )
{
	level endon( "game_ended" );
	for ( ;; )
	{
		wait 0.2;
		foreach ( player in level.players )
		{
			if ( !isAlive( player ) )
				continue;
			p = player.origin;
			if ( p[2] < lo[2] || p[0] < lo[0] || p[0] > hi[0] || p[1] < lo[1] || p[1] > hi[1] )
				player suicide();
		}
	}
}
"#
    .replace("LO", &lo)
    .replace("HI", &hi)
}
