//! Arena packages: a directory holding `arena.glb` (render geometry with
//! base-colour textures), an optional `collision.glb` (triangle soup) and
//! `arena.json` (spawns and bounds), played under IW4 rules with MW2's
//! common_mp. The package supplies geometry, collision and spawns; a donor IW4
//! map zone supplies everything a map zone has that a glTF file does not
//! (world techsets, sun, sky, vision), see `assets::mec`.

mod clip;
mod entities;
mod package;
mod world;

use std::path::{Path, PathBuf};

pub use clip::build_clip_collision;
pub use entities::{entity_string, intermission_view, map_script, minimap_corners, spawn_points};
pub use package::{ArenaImage, ArenaMesh, ArenaPackage, ArenaSpawn, ArenaTexture, BlockKind};
pub use world::{arena_fog, arena_vision, build_world_draw};

/// `map mec:<name>`.
pub const ZONE_PREFIX: &str = "mec:";
/// Map name the GSC sees (`mapname`, `maps/mp/<zone>`). Stock scripts treat
/// any map whose name lacks `mp_` as single player (`isSP`).
pub const ZONE_STEM_PREFIX: &str = "mp_mec_";

const ARENAS_ENV: &str = "IW4L_MEC_ARENAS";
const ARENAS_DIR: &str = "mec-arenas";
const DONOR_ENV: &str = "IW4L_MEC_DONOR";
const DEFAULT_DONOR: &str = "mp_highrise";

/// glTF metres to IW4 inches.
pub const INCHES_PER_METRE: f32 = 39.3701;

/// The one glTF → IW4 mapping. glTF is right-handed with +Y up and -Z forward;
/// IW4 is right-handed with +Z up, +X forward and +Y left. `iw = (-gz, -gx, gy)`
/// is a proper rotation (determinant +1), so nothing is mirrored and windings
/// keep their sense; yaw about glTF +Y is the same angle about IW4 +Z.
pub fn gltf_dir_to_iw4(v: [f32; 3]) -> [f32; 3] {
    [-v[2], -v[0], v[1]]
}

pub fn gltf_to_iw4(p: [f32; 3]) -> [f32; 3] {
    gltf_dir_to_iw4(p).map(|c| c * INCHES_PER_METRE)
}

/// `mec:testbox` → `testbox`; also accepts the GSC map name `mp_mec_testbox`.
pub fn arena_name(zone: &str) -> Option<&str> {
    let zone = zone.trim();
    let name = zone
        .strip_prefix(ZONE_PREFIX)
        .or_else(|| zone.strip_prefix(ZONE_STEM_PREFIX))?;
    (!name.is_empty() && !name.contains(['/', '\\', '.'])).then_some(name)
}

pub fn map_stem(name: &str) -> String {
    format!("{ZONE_STEM_PREFIX}{}", name.to_ascii_lowercase())
}

/// `IW4L_MEC_ARENAS`, else `<artifacts>/mec-arenas`, else the first
/// `mec-arenas` beside the executable or any of its parents, then the same
/// walk from the working directory.
pub fn arenas_root() -> PathBuf {
    if let Some(dir) = std::env::var_os(ARENAS_ENV).filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    let artifacts = std::env::var_os("IW4L_ARTIFACTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("iw4l-artifacts"))
        .join(ARENAS_DIR);
    if artifacts.is_dir() {
        return artifacts;
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    exe_dir
        .into_iter()
        .chain(std::env::current_dir().ok())
        .flat_map(|start| {
            start
                .ancestors()
                .map(|dir| dir.join(ARENAS_DIR))
                .collect::<Vec<_>>()
        })
        .find(|dir| dir.is_dir())
        .unwrap_or(artifacts)
}

/// Arena names under [`arenas_root`], sorted; `_cache` and partial builds are skipped.
pub fn list_arenas() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(arenas_root()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| is_arena_dir(&entry.path()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| arena_name(&format!("{ZONE_PREFIX}{name}")).is_some())
        .filter(|name| !name.starts_with('_'))
        .collect();
    names.sort();
    names
}

pub fn arena_dir(name: &str) -> PathBuf {
    arenas_root().join(name)
}

/// The IW4 map zone whose world techsets, sun and sky an arena borrows.
pub fn donor_zone() -> String {
    std::env::var(DONOR_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_DONOR.to_owned())
}

pub fn is_arena_dir(dir: &Path) -> bool {
    dir.join("arena.json").is_file() && dir.join("arena.glb").is_file()
}
