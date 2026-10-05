//! Arena packages (`map mec:<name>`, crate `asset_mec`) over a donor IW4 map.
//!
//! The donor zone walks through the ordinary IW4 lane, so the match keeps
//! everything a glTF file cannot carry: world techsets, sun, sky, vision, the
//! player bodies and the image/sound archives found next to it. `graft_arena`
//! then replaces what the map itself is: geometry, collision, entities and
//! spawns, and drops every donor product placed in donor space.

use std::path::{Path, PathBuf};

use asset_transport::{GamesRoot, ZoneFile, find_zone_file};

use crate::lane::LoadedWorld;

/// `mec:<name>` or `mec_<name>` → the donor zone file under the arena's map
/// name; any other zone resolves as before.
pub fn find_match_zone(games: &GamesRoot, zone: &str) -> Result<ZoneFile, String> {
    let Some(name) = asset_mec::arena_name(zone) else {
        return find_zone_file(games, zone);
    };
    let dir = asset_mec::arena_dir(name);
    if !asset_mec::is_arena_dir(&dir) {
        return Err(format!(
            "arena `{name}`: {} has no arena.json + arena.glb",
            dir.display()
        ));
    }
    let donor = asset_mec::donor_zone();
    let mut found = find_zone_file(games, &donor)
        .map_err(|error| format!("arena `{name}`: donor zone `{donor}`: {error}"))?;
    found.alias_note = Some(format!("arena `{name}` over donor `{}`", found.zone_name));
    found.zone_name = asset_mec::map_stem(name);
    Ok(found)
}

/// The arena directory a match zone name stands for.
pub fn arena_for_zone(zone: &str) -> Option<PathBuf> {
    asset_mec::arena_name(zone).map(asset_mec::arena_dir)
}

/// Menu map pack label for the arenas under `asset_mec::arenas_root`.
pub const CATALYST_PACK: &str = "CATALYST";

/// `packs` with a leading Catalyst pack (`mec:<name>` per arena) when any arena is installed.
pub fn with_catalyst_pack(
    mut packs: Vec<asset_transport::MapPack>,
) -> Vec<asset_transport::MapPack> {
    let maps: Vec<String> = asset_mec::list_arenas()
        .into_iter()
        .map(|name| format!("{}{name}", asset_mec::ZONE_PREFIX))
        .collect();
    if !maps.is_empty() {
        diag::info!(
            Launch,
            "menu: {} Catalyst arenas under {}",
            maps.len(),
            asset_mec::arenas_root().display()
        );
        packs.insert(
            0,
            asset_transport::MapPack {
                label: CATALYST_PACK.to_owned(),
                maps,
            },
        );
    }
    packs
}

/// `arena.json` `title`, else the name without its `mec_` prefix in words
/// (`mec_anchor_1` → `Anchor 1`). Cached per name.
pub fn arena_title(name: &str) -> String {
    static TITLES: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
        std::sync::Mutex::new(None);
    let mut titles = TITLES.lock().unwrap_or_else(|poison| poison.into_inner());
    titles
        .get_or_insert_with(Default::default)
        .entry(name.to_owned())
        .or_insert_with(|| {
            std::fs::read(asset_mec::arena_dir(name).join("arena.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|json| json.get("title")?.as_str().map(str::to_owned))
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| {
                    name.trim_start_matches("mec_")
                        .split('_')
                        .filter(|word| !word.is_empty())
                        .map(|word| {
                            let mut chars = word.chars();
                            chars.next().map_or_else(String::new, |first| {
                                first.to_uppercase().chain(chars).collect()
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
        })
        .clone()
}

pub(crate) fn graft_arena(loaded: &mut LoadedWorld, dir: &Path) -> Result<String, String> {
    let started = std::time::Instant::now();
    let arena = asset_mec::ArenaPackage::load(dir)?;
    let stem = asset_mec::map_stem(&dir.file_name().unwrap_or(dir.as_os_str()).to_string_lossy());
    let donor = loaded
        .world
        .draw
        .take()
        .ok_or("donor zone produced no world draw")?;
    let (draw, world_report) = asset_mec::build_world_draw(&arena, &donor, &mut loaded.materials)?;
    let clip = asset_mec::build_clip_collision(&arena)?;

    let world = &mut loaded.world;
    world.min = draw.stats.min;
    world.max = draw.stats.max;
    world.world_bounds = draw.stats.bounds;
    world.draw = Some(draw);
    world.static_model_meshes.clear();
    world.static_model_instances.clear();
    world.smodel_lighting_samples.clear();
    world.script_model_instances.clear();
    world.script_brush_models.clear();
    world.flag_descriptors.clear();
    world.script_structs.clear();
    world.dyn_ents = Default::default();
    world.fx_glass = None;
    world.intermission_view = Some(asset_mec::intermission_view(&arena));
    world.exp_fog = Some(asset_mec::arena_fog());
    let vision = asset_mec::arena_vision(world.film_vision.as_ref());
    world
        .film_visions
        .insert(format!("vision/{stem}.vision"), Ok(vision.clone()));
    world.film_vision = Some(vision);

    loaded.collision = Some(clip);
    loaded.spawns = asset_mec::spawn_points(&arena);
    loaded
        .scripts
        .set_entities(asset_mec::entity_string(&arena));
    loaded
        .scripts
        .insert_source(&format!("maps/mp/{stem}"), asset_mec::map_script(&arena));

    let facts = &mut loaded.facts;
    facts.minimap_corners = Some(asset_mec::minimap_corners(&arena));
    facts.north_yaw = Some(90.0);
    facts.compass = Default::default();
    facts.script_sound = Default::default();
    facts.airstrike_height = Some(arena.maxs[2] + 1500.0);

    loaded.report.extend(arena.report.iter().cloned());
    loaded.report.extend(world_report);
    Ok(format!(
        "arena `{}` from {}: {} spawns, {} collision tris, grafted in {:.0}ms",
        arena.name,
        dir.display(),
        arena.spawns.len(),
        arena.collision.len(),
        started.elapsed().as_secs_f32() * 1000.0
    ))
}
