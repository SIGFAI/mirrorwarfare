use std::sync::Arc;

use asset_material::{
    AssetNamespace, AssetRef, AuthoredImage, AuthoredMaterial, MaterialCatalog, MaterialDrawMode,
    TS_COLOR_MAP, TS_NORMAL_MAP, TS_SPECULAR_MAP, TechsetKey, TechsetResolve,
};
use asset_world::{
    CameraRangeKind, CameraSurfRange, CameraSurfRanges, DpvsWorldData, ExpFog, FilmVision,
    SurfaceCastsSunShadow, SurfaceDrawFields, WorldDraw, WorldLightmap, WorldMeshStats,
    WorldVertexPayload, make_material_batches, surface_pass, world_capture_from_casters,
};
use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::math::UVec2;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use dpvs_iw4::{AabbNodeView, Bounds, SurfRange, skin_dual_dvar_pack_unit_vec};

use crate::package::{cross, dot, normalize, sub};
use crate::{ArenaImage, ArenaPackage, BlockKind};

const MAX_SURFACE_TRIS: usize = 16_000;
/// Surface tile edge, inches (~32 m); `IW4L_MEC_SURFACE_TILE` overrides, 0 = untiled.
const SURFACE_TILE: f32 = 1260.0;
/// The lightmap page is a lookup table rather than a bake: across, ambient
/// light from the vertex's sky visibility and facing; down, the sun mask from
/// its sun visibility. Each vertex samples its own point, so a triangle
/// interpolates both terms linearly, like a coarse per-vertex bake.
const LIGHTMAP_AMBIENT_LEVELS: u32 = 32;
const LIGHTMAP_SUN_LEVELS: u32 = 16;
/// Lightmap ambient/directional value for open sky; `IW4L_LIGHTMAP_FLAT` uses 96.
const LIGHTMAP_PEAK: f32 = 100.0;
/// Ambient tint at open sky: Catalyst's city is lit by a bright, slightly cool sky.
const AMBIENT_TINT: [f32; 3] = [0.95, 0.99, 1.06];
/// Sun colour at unit luminance (warm white).
const SUN_TINT: [f32; 3] = [1.04, 1.0, 0.93];
/// Ambient left in fully occluded corners, as a fraction of open sky.
const AMBIENT_FLOOR: f32 = 0.22;

/// Lighting knobs; `IW4L_MEC_LIGHT="floor=0.4,peak=110,sun=bake|flat,tint=0.95/0.99/1.06"`
/// overrides them for look tuning.
#[derive(Clone, Copy)]
struct LightTune {
    floor: f32,
    peak: f32,
    /// true: the baked sun visibility masks the sun; false: the mask is open
    /// and the sun shadow maps alone shade it.
    baked_sun: bool,
    tint: [f32; 3],
}

impl LightTune {
    fn get() -> Self {
        let mut t = Self {
            floor: AMBIENT_FLOOR,
            peak: LIGHTMAP_PEAK,
            baked_sun: true,
            tint: AMBIENT_TINT,
        };
        let Ok(spec) = std::env::var("IW4L_MEC_LIGHT") else {
            return t;
        };
        for item in spec.split(',') {
            let Some((k, v)) = item.split_once('=') else {
                continue;
            };
            let f = v.trim().parse::<f32>().ok();
            match (k.trim(), f) {
                ("floor", Some(f)) => t.floor = f.clamp(0.0, 1.0),
                ("peak", Some(f)) => t.peak = f.clamp(16.0, 255.0),
                ("sun", _) => t.baked_sun = v.trim() != "flat",
                ("tint", _) => {
                    let c: Vec<f32> = v.split('/').filter_map(|x| x.trim().parse().ok()).collect();
                    if c.len() == 3 {
                        t.tint = [c[0], c[1], c[2]];
                    }
                }
                _ => {}
            }
        }
        t
    }
}

/// What the arena borrows from the donor world: one simple lit opaque world
/// material to clone per texture, its sun light and reflection probe, and the
/// donor's sky material for a box around the arena.
struct Donor {
    material: usize,
    /// An alpha-tested sibling for cut-out textures (fences, foliage).
    cutout_material: Option<usize>,
    /// An alpha-blended lit world material for glass and decals.
    blend_material: Option<usize>,
    sibling_rejects: String,
    primary_light: u8,
    reflection_probe: u8,
    sky_material: Option<usize>,
}

/// Arena render geometry as an IW4 world: every surface in one DPVS cell with
/// no portals, lit by a flat fully sunlit lightmap page so the donor techset
/// adds the sun term per pixel. The donor world's surfaces, static models and
/// local lights are dropped; its sun, probes and sky material are kept.
pub fn build_world_draw(
    arena: &ArenaPackage,
    donor: &WorldDraw,
    catalog: &mut MaterialCatalog,
) -> Result<(WorldDraw, Vec<String>), String> {
    let mut report = Vec::new();
    let picked = pick_donor(donor, catalog)?;
    report.push(format!(
        "donor cut-out/blend candidates: {}",
        picked.sibling_rejects
    ));
    report.push(format!(
        "donor material `{}` (techset `{}`), cut-out `{}`, blend `{}`, primary light {}, probe {}, sky {}",
        catalog.materials[picked.material].name.as_str(),
        catalog.materials[picked.material].technique_set.as_str(),
        picked.cutout_material.map_or("none".to_owned(), |m| format!(
            "{} ({})",
            catalog.materials[m].name.as_str(),
            catalog.materials[m].technique_set.as_str()
        )),
        picked.blend_material.map_or("none".to_owned(), |m| format!(
            "{} ({})",
            catalog.materials[m].name.as_str(),
            catalog.materials[m].technique_set.as_str()
        )),
        picked.primary_light,
        picked.reflection_probe,
        picked
            .sky_material
            .map_or("none".to_owned(), |m| catalog.materials[m]
                .name
                .as_str()
                .to_owned()),
    ));

    // Normal maps are read DXT5nm-style: x from alpha, y from green.
    let flat = |catalog: &mut MaterialCatalog, template: usize| {
        (
            push_image(
                catalog,
                template,
                TS_NORMAL_MAP,
                "mec/flat_normal",
                Payload::Rgba(4, 4, [128, 129, 255, 130].repeat(16)),
            ),
            push_image(
                catalog,
                template,
                TS_SPECULAR_MAP,
                "mec/dull_spec",
                Payload::Rgba(4, 4, [24, 24, 24, 160].repeat(16)),
            ),
        )
    };
    let mut flats: std::collections::HashMap<usize, (Option<usize>, Option<usize>)> =
        Default::default();
    let mut texture_materials = Vec::with_capacity(arena.textures.len());
    let (mut cutouts, mut blends, mut normals, mut speculars, mut compressed) =
        (0, 0, 0, 0, 0usize);
    for (i, texture) in arena.textures.iter().enumerate() {
        let name = format!("mec/{}/{i}_{}", arena.name, sanitize(&texture.name));
        let template = match (picked.cutout_material, picked.blend_material) {
            (Some(cutout), _) if texture.alpha_test => {
                cutouts += 1;
                cutout
            }
            (_, Some(blend)) if texture.blend => {
                blends += 1;
                blend
            }
            _ => picked.material,
        };
        let payload = match &texture.compressed {
            Some(image) => {
                compressed += 1;
                Payload::Blocks(image)
            }
            None => {
                let (width, height) = fit_u16(texture.width, texture.height);
                let rgba = if (width, height) == (texture.width, texture.height) {
                    texture.rgba.clone()
                } else {
                    resample(&texture.rgba, texture.width, texture.height, width, height)
                };
                Payload::Rgba(width, height, rgba)
            }
        };
        let color = push_image(catalog, template, TS_COLOR_MAP, &name, payload)
            .ok_or("donor material lost its decoded colour map")?;
        let (flat_normal, dull_spec) = *flats
            .entry(template)
            .or_insert_with(|| flat(catalog, template));
        let normal = texture.normal.as_deref().and_then(|image| {
            push_image(
                catalog,
                template,
                TS_NORMAL_MAP,
                &format!("{name}_n"),
                Payload::Blocks(image),
            )
        });
        let specular = texture.specular.as_deref().and_then(|image| {
            push_image(
                catalog,
                template,
                TS_SPECULAR_MAP,
                &format!("{name}_s"),
                Payload::Blocks(image),
            )
        });
        normals += usize::from(normal.is_some());
        speculars += usize::from(specular.is_some());
        let mut material = catalog.materials[template].clone();
        material.name = AssetRef::Real(name);
        for binding in &mut material.textures {
            binding.image = match binding.semantic {
                TS_COLOR_MAP => Some(color),
                TS_NORMAL_MAP => normal.or(flat_normal).or(binding.image),
                TS_SPECULAR_MAP => specular.or(dull_spec).or(binding.image),
                _ => binding.image,
            };
        }
        catalog.materials.push(material);
        texture_materials.push(catalog.materials.len() - 1);
    }
    report.push(format!(
        "arena materials: {} ({compressed} block-compressed colour maps, {normals} normal maps, {speculars} specular maps, {cutouts} cut-out, {blends} blended)",
        texture_materials.len()
    ));

    let tune = LightTune::get();
    let mut geo = Geometry::default();
    let mut surface_materials = Vec::new();
    let mut lit_surfaces = 0usize;
    let baked = arena.meshes.iter().filter(|m| !m.colors.is_empty()).count();
    let tile = std::env::var("IW4L_MEC_SURFACE_TILE")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .map_or(SURFACE_TILE, |v| if v > 0.0 { v } else { f32::MAX });
    // Opaque first, cut-outs after them, blended last (their own LitTrans
    // range), so batches follow sort order.
    let class = |m: &crate::ArenaMesh| {
        let t = &arena.textures[m.material];
        if picked.blend_material.is_some() && t.blend {
            2
        } else if picked.cutout_material.is_some() && t.alpha_test {
            1
        } else {
            0
        }
    };
    let mut meshes: Vec<&crate::ArenaMesh> = arena.meshes.iter().collect();
    meshes.sort_by_key(|m| class(m));
    let mut opaque_surfaces = None;
    for mesh in meshes {
        if class(mesh) == 2 && opaque_surfaces.is_none() {
            opaque_surfaces = Some(surface_materials.len());
        }
        let base = geo.positions.len() as u32;
        let tangents = vertex_tangents(mesh);
        for v in 0..mesh.positions.len() {
            let lm = lightmap_coord(mesh, v, arena.sun_dir, tune);
            geo.push_vertex(
                mesh.positions[v],
                mesh.normals[v],
                tangents[v],
                mesh.uvs[v],
                lm,
            );
        }
        // IW4 world triangles are clockwise seen from the front. Surfaces are
        // cut per tile so DPVS can frustum-cull them by their bounds.
        let mut tiles: std::collections::BTreeMap<(i32, i32), Vec<[u32; 3]>> = Default::default();
        for t in mesh.indices.chunks_exact(3) {
            let c = [0, 1].map(|a| {
                let sum: f32 = t.iter().map(|&i| mesh.positions[i as usize][a]).sum();
                (sum / 3.0 / tile).floor() as i32
            });
            tiles
                .entry((c[0], c[1]))
                .or_default()
                .push([base + t[0], base + t[2], base + t[1]]);
        }
        for tris in tiles.values() {
            for chunk in tris.chunks(MAX_SURFACE_TRIS) {
                geo.push_surface(chunk);
                surface_materials.push(Some(texture_materials[mesh.material]));
                lit_surfaces += 1;
            }
        }
    }
    if lit_surfaces == 0 {
        return Err("arena.glb produced no surfaces".into());
    }
    if let Some(sky) = picked.sky_material {
        let centre = [0, 1, 2].map(|a| (arena.mins[a] + arena.maxs[a]) * 0.5);
        let reach = (0..3)
            .map(|a| arena.maxs[a] - arena.mins[a])
            .fold(0.0f32, f32::max)
            * 2.0
            + 4096.0;
        geo.push_sky_box(centre, reach);
        surface_materials.push(Some(sky));
    }
    let surface_count = surface_materials.len();
    if surface_count > u16::MAX as usize {
        return Err(format!("arena has {surface_count} surfaces; at most 65535"));
    }

    let lightmapped: Vec<bool> = (0..surface_count).map(|s| s < lit_surfaces).collect();
    let lightmap_indices = vec![0u8; surface_count];
    let primary_lights: Vec<u8> = (0..surface_count)
        .map(|s| {
            if s < lit_surfaces {
                picked.primary_light
            } else {
                0
            }
        })
        .collect();
    let probes = vec![picked.reflection_probe; surface_count];
    let (batches, surface_batch_ranges) = make_material_batches(
        &geo.positions,
        &geo.normals,
        &geo.tangents,
        &geo.colors,
        &geo.uvs,
        &geo.lightmap_uvs,
        &geo.indices,
        &geo.surface_ranges,
        &surface_materials,
        &lightmapped,
        &lightmap_indices,
        &primary_lights,
        &probes,
    );
    let surface_draw_fields: Vec<SurfaceDrawFields> = geo
        .surface_ranges
        .iter()
        .enumerate()
        .map(|(s, &(start, count))| SurfaceDrawFields {
            first_vertex: 0,
            tri_count: (count / 3) as u16,
            base_index: start,
            lightmap_index: 0,
            reflection_probe_index: picked.reflection_probe,
            primary_light_index: primary_lights[s],
        })
        .collect();

    let opaque_surfaces = opaque_surfaces.unwrap_or(lit_surfaces);
    let mut casters = SurfaceCastsSunShadow::with_len(surface_count);
    for s in 0..opaque_surfaces {
        casters.set(s);
    }
    let sky_surfs: Vec<u32> = (lit_surfaces as u32..surface_count as u32).collect();
    let dpvs = single_cell_dpvs(
        &geo,
        opaque_surfaces,
        lit_surfaces,
        surface_count,
        picked.reflection_probe,
    );

    let (mut min, mut max) = (arena.mins, arena.maxs);
    for p in &geo.positions[..geo.lit_vertex_count.unwrap_or(geo.positions.len())] {
        for a in 0..3 {
            min[a] = min[a].min(p[a]);
            max[a] = max[a].max(p[a]);
        }
    }
    let sun_count = donor.sun_primary_light_count as usize;
    let mut kept_lights: Vec<_> = donor
        .primary_lights
        .iter()
        .take(sun_count + 1)
        .cloned()
        .collect();
    // The donor's sun shines from the direction the arena was baked for.
    let to_sun = arena.sun_dir.map(|c| -c);
    for light in kept_lights.iter_mut().filter(|l| l.is_sun) {
        report.push(format!(
            "sun: donor direction {:?} colour {:?} -> arena {:?}",
            light.direction.map(|c| (c * 1000.0).round() / 1000.0),
            light.color.map(|c| (c * 1000.0).round() / 1000.0),
            to_sun.map(|c| (c * 1000.0).round() / 1000.0)
        ));
        light.direction = to_sun;
        // Keep the donor's sun strength, not its tint: a near-white noon sun.
        let luma = 0.2126 * light.color[0] + 0.7152 * light.color[1] + 0.0722 * light.color[2];
        light.color = SUN_TINT.map(|c| c * luma);
    }
    report.push(format!(
        "arena world: {} vertices, {} triangles, {lit_surfaces} lit surfaces, {} sky surfaces, {} materials ({cutouts} cut-out), {baked}/{} meshes baked",
        geo.positions.len(),
        geo.indices.len() / 3,
        sky_surfs.len(),
        texture_materials.len(),
        arena.meshes.len(),
    ));

    Ok((
        WorldDraw {
            sky_model: None,
            batches,
            lightmap: Ok(vec![Some(ramp_lightmap(tune))]),
            stats: WorldMeshStats {
                vertices: geo.positions.len(),
                triangles: geo.indices.len() / 3,
                surfaces: surface_count,
                sky_surfaces: sky_surfs.len(),
                sky_material: picked.sky_material,
                min,
                max,
                // GfxWorld bounds: midpoint, half size.
                bounds: Some([
                    (min[0] + max[0]) * 0.5,
                    (min[1] + max[1]) * 0.5,
                    (min[2] + max[2]) * 0.5,
                    (max[0] - min[0]) * 0.5,
                    (max[1] - min[1]) * 0.5,
                    (max[2] - min[2]) * 0.5,
                ]),
                ..WorldMeshStats::default()
            },
            packed_vertices: WorldVertexPayload::Iw4(geo.packed),
            vertex_layer: Vec::new(),
            surface_vertex_layer: Vec::new(),
            surface_first_vertex: vec![0; surface_count],
            surface_draw_fields,
            positions: geo.positions,
            normals: geo.normals,
            tangents: geo.tangents,
            colors: geo.colors,
            texture_uvs: geo.uvs,
            lightmap_uvs: geo.lightmap_uvs,
            packed_indices: geo.indices,
            surface_index_ranges: geo.surface_ranges,
            surface_batch_ranges,
            surface_lightmapped: lightmapped,
            surface_lightmap_indices: lightmap_indices,
            surface_reflection_probes: probes,
            surface_primary_lights: primary_lights,
            sort_key_distortion: donor.sort_key_distortion,
            capture: world_capture_from_casters(casters),
            brush_models: Vec::new(),
            brush_model_bounds: Vec::new(),
            surface_materials,
            light_region_hulls: donor
                .light_region_hulls
                .as_ref()
                .map(|hulls| hulls.iter().take(kept_lights.len()).cloned().collect()),
            primary_lights: kept_lights,
            light_defs: donor.light_defs.clone(),
            sun_primary_light_count: donor.sun_primary_light_count,
            sun_stages: donor.sun_stages.clone(),
            shadow_geometry: Vec::new(),
            reflection_probes: donor.reflection_probes.clone(),
            dpvs,
            outdoor_image_name: donor.outdoor_image_name.clone(),
            outdoor_image: None,
            outdoor_lookup: donor.outdoor_lookup,
            sun_effects: donor.sun_effects.clone(),
            t5_sun_parse_exposure: None,
            t5_sky_dynamic_intensity: None,
            t5_sun_light: None,
            t5_tree_scatter_intensity: None,
            t5_tree_scatter_amount: None,
            t5_exposure_volume_count: 0,
        },
        report,
    ))
}

fn pick_donor(donor: &WorldDraw, catalog: &MaterialCatalog) -> Result<Donor, String> {
    let sun_count = donor.sun_primary_light_count as u8;
    let mut candidates = std::collections::BTreeMap::new();
    let mut sky_material = None;
    let mut rejected = std::collections::BTreeMap::new();
    for (s, material) in donor.surface_materials.iter().enumerate() {
        let Some(index) = *material else {
            continue;
        };
        let Some(m) = catalog.materials.get(index) else {
            continue;
        };
        if sky_material.is_none() && surface_pass(catalog, Some(m)).sky {
            sky_material = Some(index);
        }
        let light = donor.surface_primary_lights.get(s).copied().unwrap_or(0);
        if !donor.surface_lightmapped.get(s).copied().unwrap_or(false) {
            *rejected.entry("not lightmapped").or_insert(0) += 1;
            continue;
        }
        if light == 0 || light > sun_count {
            *rejected.entry("not sunlit").or_insert(0) += 1;
            continue;
        }
        if let Err(reason) = simple_lit(catalog, m, false) {
            *rejected.entry(reason).or_insert(0usize) += 1;
            continue;
        }
        let tris = donor
            .surface_index_ranges
            .get(s)
            .map_or(0, |r| r.1 as usize / 3);
        let probe = donor.surface_reflection_probes.get(s).copied().unwrap_or(0);
        let probe_ok = donor
            .reflection_probes
            .get(probe as usize)
            .is_some_and(|p| p.image.is_some());
        if !probe_ok {
            *rejected.entry("probe").or_insert(0) += 1;
            continue;
        }
        let rank = u8::from(catalog.agreed_draw_mode(m) == Some(MaterialDrawMode::Opaque));
        let entry = candidates
            .entry(index)
            .or_insert((rank, 0usize, light, probe));
        entry.1 += tris;
    }
    let (&material, &(_, _, primary_light, reflection_probe)) = candidates
        .iter()
        .max_by_key(|(_, (rank, tris, ..))| (*rank, *tris))
        .ok_or_else(|| {
            format!(
                "donor world has no sunlit, lightmapped, opaque colour-map material to clone                  (surfaces rejected: {rejected:?}); try another IW4L_MEC_DONOR"
            )
        })?;
    let mode_of = |m: usize| lit_draw_mode(catalog, &catalog.materials[m]);
    // Cut-out and blended siblings: first the donor's own sunlit surfaces,
    // then any loaded world material with the same simple lit texture set.
    let wide = |want: fn(Option<MaterialDrawMode>) -> bool| {
        candidates
            .iter()
            .filter(|&(&m, _)| want(mode_of(m)))
            .max_by_key(|(_, (_, tris, ..))| *tris)
            .map(|(&m, _)| m)
            .or_else(|| {
                (0..catalog.materials.len()).find(|&m| {
                    want(mode_of(m)) && simple_lit(catalog, &catalog.materials[m], true).is_ok()
                })
            })
    };
    let cutout_material = wide(|mode| matches!(mode, Some(MaterialDrawMode::AlphaTest { .. })));
    let blend_material = wide(|mode| matches!(mode, Some(MaterialDrawMode::Blend)));
    // Why the loaded alpha-tested / blended world materials were not usable.
    let mut sibling_rejects: std::collections::BTreeMap<String, usize> = Default::default();
    for m in &catalog.materials {
        let mode = lit_draw_mode(catalog, m);
        let label = match mode {
            Some(MaterialDrawMode::AlphaTest { .. }) => "alpha-test",
            Some(MaterialDrawMode::Blend) => "blend",
            _ => continue,
        };
        let why = simple_lit(catalog, m, true).err().unwrap_or("ok");
        *sibling_rejects
            .entry(format!("{label}: {why}"))
            .or_insert(0) += 1;
    }
    Ok(Donor {
        sibling_rejects: format!("{sibling_rejects:?}"),
        material,
        cutout_material,
        blend_material,
        primary_light,
        reflection_probe,
        sky_material,
    })
}

/// The draw mode of a material's lit technique (the one an arena surface
/// draws with). Many world materials disagree across their colour
/// techniques (depth prepass, lit, debug), so the catalog-wide agreement is
/// often absent where the lit band alone is clear.
fn lit_draw_mode(catalog: &MaterialCatalog, m: &AuthoredMaterial) -> Option<MaterialDrawMode> {
    if let Some(mode) = catalog.agreed_draw_mode(m) {
        return Some(mode);
    }
    let slots = m.route?.technique_slots;
    [37usize, 38, 39].into_iter().find_map(|tech| {
        asset_iw4::color_pass_row_for_tech_type(
            m.state_bits_entry.as_ref(),
            &m.state_bits,
            slots,
            tech,
        )
        .map(MaterialDrawMode::from_state_bits)
    })
}

/// An opaque lit world material whose only textures are colour, normal and
/// specular maps, on a single-layer world vertex.
fn simple_lit(
    catalog: &MaterialCatalog,
    m: &AuthoredMaterial,
    allow_blend: bool,
) -> Result<(), &'static str> {
    if m.namespace != AssetNamespace::Iw4 || !m.name.is_real() {
        return Err("namespace/name");
    }
    let pass = surface_pass(catalog, Some(m));
    if !pass.takes_lightmap() || pass.unrouted {
        return Err("pass");
    }
    if m.route.is_none_or(|route| route.takes_model_lighting()) {
        return Err("route");
    }
    // Alpha test is harmless under fully opaque arena textures; blending is
    // only for the blended (glass, decal) sibling.
    // The opaque base must use the catalog-wide agreement: the lit-band
    // fallback reads many ordinary world materials as blended.
    let mode = if allow_blend {
        lit_draw_mode(catalog, m)
    } else {
        catalog.agreed_draw_mode(m)
    };
    match mode {
        Some(MaterialDrawMode::Blend | MaterialDrawMode::AlphaTest { .. }) if allow_blend => {}
        Some(
            MaterialDrawMode::Blend
            | MaterialDrawMode::Multiply
            | MaterialDrawMode::Additive
            | MaterialDrawMode::Screen,
        ) => return Err("draw mode"),
        _ => {}
    }
    let TechsetResolve::Hit { facts, .. } =
        catalog.resolve_technique_set(TechsetKey::new(m.namespace, m.technique_set.as_str()))
    else {
        return Err("techset");
    };
    if facts.world_vert_format != 0 {
        return Err("layered vertex");
    }
    let mut color = false;
    for t in &m.textures {
        match t.semantic {
            TS_COLOR_MAP if !color => color = true,
            TS_NORMAL_MAP | TS_SPECULAR_MAP => {}
            _ => return Err("texture set"),
        }
        let image = t
            .image
            .and_then(|i| catalog.images.get(i))
            .ok_or("image row")?;
        if image.map_type != 3 {
            return Err("image type");
        }
    }
    if color { Ok(()) } else { Err("no colour map") }
}

/// Appends a decoded image modelled on the donor material's binding of the
/// same semantic; `None` when the donor has no such binding.
enum Payload<'a> {
    Rgba(u32, u32, Vec<u8>),
    /// Block-compressed with its mip chain: uploaded as stored, no decode.
    Blocks(&'a ArenaImage),
}

fn push_image(
    catalog: &mut MaterialCatalog,
    donor: usize,
    semantic: u8,
    name: &str,
    payload: Payload<'_>,
) -> Option<usize> {
    let slot = catalog.materials[donor]
        .textures
        .iter()
        .find(|t| t.semantic == semantic)?
        .image?;
    let sampler_state = catalog.materials[donor]
        .textures
        .iter()
        .find(|t| t.semantic == semantic)?
        .sampler_state;
    let template: &AuthoredImage = catalog.images.get(slot)?;
    // Opaque lit colour maps decode to a linear view of sRGB bytes; keep a
    // decoded donor's choice when there is one.
    let srgb = template
        .decoded
        .as_ref()
        .is_some_and(|image| image.texture_descriptor.format.is_srgb());
    let extent = |width, height| Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let (mut image, width, height, levels) = match payload {
        Payload::Rgba(width, height, rgba) => {
            let format = if srgb {
                TextureFormat::Rgba8UnormSrgb
            } else {
                TextureFormat::Rgba8Unorm
            };
            let (data, levels) = mip_chain(rgba, width, height);
            let mut image = Image::new(
                extent(width, height),
                TextureDimension::D2,
                data[..(width * height * 4) as usize].to_vec(),
                format,
                RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
            );
            image.data = Some(data);
            (image, width, height, levels)
        }
        Payload::Blocks(source) => {
            let format = match (source.kind, srgb) {
                (BlockKind::Bc1, false) => TextureFormat::Bc1RgbaUnorm,
                (BlockKind::Bc1, true) => TextureFormat::Bc1RgbaUnormSrgb,
                (BlockKind::Bc3, false) => TextureFormat::Bc3RgbaUnorm,
                (BlockKind::Bc3, true) => TextureFormat::Bc3RgbaUnormSrgb,
                (BlockKind::Bc7, false) => TextureFormat::Bc7RgbaUnorm,
                (BlockKind::Bc7, true) => TextureFormat::Bc7RgbaUnormSrgb,
            };
            if source.width > u32::from(u16::MAX) || source.height > u32::from(u16::MAX) {
                return None;
            }
            let mut image = Image::new_uninit(
                extent(source.width, source.height),
                TextureDimension::D2,
                format,
                RenderAssetUsages::RENDER_WORLD,
            );
            image.data = Some(source.data.clone());
            (image, source.width, source.height, source.levels)
        }
    };
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = bevy::image::ImageSampler::Descriptor(asset_material::sampler_from_iw4(
        sampler_state,
        levels,
        false,
    ));
    let row = AuthoredImage {
        name: AssetRef::Real(name.to_owned()),
        width: width as u16,
        height: height as u16,
        depth: 1,
        level_count: levels as u8,
        payload: Arc::new(Vec::new()),
        decoded: Some(Arc::new(image)),
        common_owned: false,
        decoded_variant: None,
        decoded_by: None,
        pending_decode: None,
        ..template.clone()
    };
    catalog.images.push(row);
    Some(catalog.images.len() - 1)
}

fn mip_chain(mut level: Vec<u8>, mut w: u32, mut h: u32) -> (Vec<u8>, u32) {
    let mut out = level.clone();
    let mut levels = 1;
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let mut sum = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        sum += u32::from(level[((sy * w + sx) * 4 + c) as usize]);
                    }
                    next[((y * nw + x) * 4 + c) as usize] = ((sum + 2) / 4) as u8;
                }
            }
        }
        out.extend_from_slice(&next);
        level = next;
        (w, h) = (nw, nh);
        levels += 1;
    }
    (out, levels)
}

/// Largest texture edge kept; a district can carry a hundred 2k maps.
/// `IW4L_MEC_MAX_TEXTURE` overrides.
fn max_texture_edge() -> u32 {
    std::env::var("IW4L_MEC_MAX_TEXTURE")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|&v: &u32| (4..=4096).contains(&v))
        .unwrap_or(1024)
}

fn fit_u16(w: u32, h: u32) -> (u32, u32) {
    let scale = (w.max(h) as f32 / max_texture_edge() as f32).max(1.0);
    (
        ((w as f32 / scale) as u32).max(1),
        ((h as f32 / scale) as u32).max(1),
    )
}

/// Area (box) filter: each target texel averages the source texels it covers.
fn resample(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dw * dh * 4) as usize);
    for y in 0..dh {
        let (y0, y1) = (
            y * sh / dh,
            ((y + 1) * sh).div_ceil(dh).clamp(y * sh / dh + 1, sh),
        );
        for x in 0..dw {
            let (x0, x1) = (
                x * sw / dw,
                ((x + 1) * sw).div_ceil(dw).clamp(x * sw / dw + 1, sw),
            );
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * sw + sx) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += u32::from(src[i + c]);
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            out.extend(sum.map(|v| ((v + n / 2) / n) as u8));
        }
    }
    out
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Where a vertex samples the lookup page: ambient from sky visibility (open
/// sky above counts more than the horizon), sun mask from sun visibility.
/// Unbaked meshes get open sky, full sun, and a facing ramp for some depth.
fn lightmap_coord(
    mesh: &crate::ArenaMesh,
    v: usize,
    sun_dir: [f32; 3],
    tune: LightTune,
) -> [f32; 2] {
    let n = mesh.normals[v];
    let (ambient, sun) = match mesh.colors.get(v) {
        Some(c) => {
            let sky = c[0].clamp(0.0, 1.0);
            let up = 0.85 + 0.15 * n[2];
            (
                tune.floor + (1.0 - tune.floor) * sky * up,
                if tune.baked_sun {
                    c[3].clamp(0.0, 1.0)
                } else {
                    1.0
                },
            )
        }
        None => (0.5 + 0.5 * (-dot(n, sun_dir)).max(0.0), 1.0),
    };
    let at = |t: f32, levels: u32| (0.5 + t * (levels - 1) as f32) / levels as f32;
    [
        at(ambient, LIGHTMAP_AMBIENT_LEVELS),
        at(sun, LIGHTMAP_SUN_LEVELS),
    ]
}

fn ramp_lightmap(tune: LightTune) -> WorldLightmap {
    let (w, h) = (LIGHTMAP_AMBIENT_LEVELS, LIGHTMAP_SUN_LEVELS);
    let image = |w: u32, h: u32, format: TextureFormat, data: Vec<u8>| {
        let mut image = Image::new(
            Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            data,
            format,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        image.sampler = bevy::image::ImageSampler::linear();
        image
    };
    let sun_mask: Vec<u8> = (0..h)
        .flat_map(|j| {
            let v = (255.0 * j as f32 / (h - 1) as f32).round() as u8;
            std::iter::repeat_n(v, w as usize)
        })
        .collect();
    let sun = image(w, h, TextureFormat::R8Unorm, sun_mask);
    let row: Vec<u8> = (0..w)
        .flat_map(|k| {
            // The techset squares the lightmap term; the ramp is linear after it.
            let v = tune.peak * (k as f32 / (w - 1) as f32).sqrt();
            let c = tune
                .tint
                .map(|t| (v * t.sqrt()).round().clamp(0.0, 255.0) as u8);
            [c[0], c[1], c[2], 128]
        })
        .collect();
    // Ambient varies across only, so it reads the same at any height in the page.
    let page = row.repeat(h as usize);
    WorldLightmap {
        primary_image: Some(sun.clone()),
        secondary_image: Some(image(w, h * 2, TextureFormat::Rgba8Unorm, page.repeat(2))),
        secondary_b_image: None,
        ambient_image: image(w, h, TextureFormat::Rgba8Unorm, page.clone()),
        directional_image: image(w, h, TextureFormat::Rgba8Unorm, page),
        sun_mask_image: sun,
        ambient_source_name: "mec ramp lightmap".to_owned(),
        sun_mask_source_name: "mec sun mask".to_owned(),
        ambient_size: UVec2::new(w, h),
        sun_mask_size: UVec2::new(w, h),
    }
}

fn vertex_tangents(mesh: &crate::ArenaMesh) -> Vec<[f32; 4]> {
    let n = mesh.positions.len();
    let mut tan = vec![[0.0f32; 3]; n];
    let mut bit = vec![[0.0f32; 3]; n];
    for t in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|k| t[k] as usize);
        let (p0, p1, p2) = (mesh.positions[a], mesh.positions[b], mesh.positions[c]);
        let (w0, w1, w2) = (mesh.uvs[a], mesh.uvs[b], mesh.uvs[c]);
        let (e1, e2) = (sub(p1, p0), sub(p2, p0));
        let (du1, dv1, du2, dv2) = (w1[0] - w0[0], w1[1] - w0[1], w2[0] - w0[0], w2[1] - w0[1]);
        let det = du1 * dv2 - du2 * dv1;
        if det.abs() < 1e-12 {
            continue;
        }
        let r = 1.0 / det;
        let sdir = [0, 1, 2].map(|k| (e1[k] * dv2 - e2[k] * dv1) * r);
        let tdir = [0, 1, 2].map(|k| (e2[k] * du1 - e1[k] * du2) * r);
        for v in [a, b, c] {
            for k in 0..3 {
                tan[v][k] += sdir[k];
                bit[v][k] += tdir[k];
            }
        }
    }
    (0..n)
        .map(|v| {
            let nrm = mesh.normals[v];
            let mut t = sub(tan[v], nrm.map(|c| c * dot(nrm, tan[v])));
            if dot(t, t) < 1e-12 {
                t = if nrm[2].abs() < 0.9 {
                    cross([0.0, 0.0, 1.0], nrm)
                } else {
                    cross(nrm, [0.0, 1.0, 0.0])
                };
            }
            let t = normalize(t);
            let sign = if dot(cross(nrm, t), bit[v]) >= 0.0 {
                1.0
            } else {
                -1.0
            };
            [t[0], t[1], t[2], sign]
        })
        .collect()
}

#[derive(Default)]
struct Geometry {
    packed: Vec<[u8; 44]>,
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    tangents: Vec<[f32; 4]>,
    colors: Vec<[f32; 4]>,
    uvs: Vec<[f32; 2]>,
    lightmap_uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    surface_ranges: Vec<(u32, u32)>,
    surface_bounds: Vec<([f32; 3], [f32; 3])>,
    lit_vertex_count: Option<usize>,
}

impl Geometry {
    fn push_vertex(&mut self, p: [f32; 3], n: [f32; 3], t: [f32; 4], uv: [f32; 2], lm: [f32; 2]) {
        let mut row = [0u8; 44];
        for (k, v) in [p[0], p[1], p[2], t[3]].into_iter().enumerate() {
            row[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        row[16..20].copy_from_slice(&[0xff; 4]);
        for (k, v) in [uv[0], uv[1], lm[0], lm[1]].into_iter().enumerate() {
            row[20 + k * 4..24 + k * 4].copy_from_slice(&v.to_le_bytes());
        }
        row[36..40].copy_from_slice(&skin_dual_dvar_pack_unit_vec(n, 1.0).to_le_bytes());
        row[40..44]
            .copy_from_slice(&skin_dual_dvar_pack_unit_vec([t[0], t[1], t[2]], 1.0).to_le_bytes());
        self.packed.push(row);
        self.positions.push(p);
        self.normals.push(n);
        self.tangents.push(t);
        self.colors.push([1.0; 4]);
        self.uvs.push(uv);
        self.lightmap_uvs.push(lm);
    }

    fn push_surface(&mut self, tris: &[[u32; 3]]) {
        let start = self.indices.len() as u32;
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for tri in tris {
            for &i in tri {
                let p = self.positions[i as usize];
                for a in 0..3 {
                    lo[a] = lo[a].min(p[a]);
                    hi[a] = hi[a].max(p[a]);
                }
            }
            self.indices.extend_from_slice(tri);
        }
        self.surface_ranges
            .push((start, self.indices.len() as u32 - start));
        self.surface_bounds.push((lo, hi));
    }

    /// Inward-facing box for the donor sky material.
    fn push_sky_box(&mut self, c: [f32; 3], r: f32) {
        self.lit_vertex_count = Some(self.positions.len());
        let base = self.positions.len() as u32;
        let corner = |i: u32| {
            [
                c[0] + if i & 1 != 0 { r } else { -r },
                c[1] + if i & 2 != 0 { r } else { -r },
                c[2] + if i & 4 != 0 { r } else { -r },
            ]
        };
        for i in 0..8 {
            let p = corner(i);
            let n = normalize(sub(c, p));
            self.push_vertex(p, n, [1.0, 0.0, 0.0, 1.0], [0.0, 0.0], [0.5, 0.5]);
        }
        let faces = [
            [0, 1, 3, 2],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 3, 7, 6],
            [0, 2, 6, 4],
            [1, 3, 7, 5],
        ];
        let mut tris = Vec::new();
        for f in faces {
            let [a, b, cc, d] = f.map(|k| base + k);
            for [x, y, z] in [[a, b, cc], [a, cc, d]] {
                let [p, q, s] = [x, y, z].map(|i| self.positions[i as usize]);
                // Clockwise seen from the inside, where the camera is.
                let inward = dot(cross(sub(q, p), sub(s, p)), sub(c, p)) > 0.0;
                tris.push(if inward { [x, z, y] } else { [x, y, z] });
            }
        }
        self.push_surface(&tris);
    }
}

fn single_cell_dpvs(
    geo: &Geometry,
    opaque: usize,
    lit: usize,
    total: usize,
    probe: u8,
) -> DpvsWorldData {
    let mut rest = Vec::new();
    if lit > opaque {
        rest.push(CameraSurfRange {
            kind: CameraRangeKind::LitTrans,
            begin: opaque as u32,
            end: lit as u32,
        });
    }
    if total > lit {
        rest.push(CameraSurfRange {
            kind: CameraRangeKind::Emissive,
            begin: lit as u32,
            end: total as u32,
        });
    }
    let ranges = CameraSurfRanges::new(
        CameraSurfRange {
            kind: CameraRangeKind::LitOpaque,
            begin: 0,
            end: opaque as u32,
        },
        rest,
    );
    let mut dpvs = DpvsWorldData::new(ranges);
    let surface_bounds: Vec<Bounds> = geo
        .surface_bounds
        .iter()
        .map(|&(lo, hi)| Bounds::from_mins_maxs(lo, hi))
        .collect();
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for &(a, b) in &geo.surface_bounds {
        for k in 0..3 {
            lo[k] = lo[k].min(a[k]);
            hi[k] = hi[k].max(b[k]);
        }
    }
    let n = total as u16;
    dpvs.planes = Vec::new();
    dpvs.nodes = vec![1];
    dpvs.cell_count = 1;
    dpvs.cell_roots = vec![SurfRange { start: 0, count: n }];
    dpvs.aabb_trees = vec![vec![AabbNodeView {
        bounds: Bounds::from_mins_maxs(lo, hi),
        child_count: 0,
        children_offset: 0,
        start_surf: 0,
        surface_count: n,
        start_surf_no_decal: 0,
        surface_count_no_decal: n,
        smodel_index_start: 0,
        smodel_index_count: 0,
    }]];
    dpvs.aabb_smodel_indices = vec![Vec::new()];
    dpvs.sorted_surf_index = (0..n).chain(0..n).collect();
    dpvs.static_surface_count = total;
    dpvs.static_surface_count_no_decal = total;
    dpvs.surface_bounds = surface_bounds;
    dpvs.portals_per_cell = vec![Vec::new()];
    dpvs.cell_reflection_probes = vec![vec![probe]];
    dpvs.lit_opaque_begin = 0;
    dpvs.lit_opaque_end = opaque as u32;
    dpvs.emissive_surfs_begin = lit as u32;
    dpvs.emissive_surfs_end = total as u32;
    dpvs.sky_start_surfs = (lit as u32..total as u32).collect();
    dpvs
}

/// Catalyst's city reads bright, clean and cool: a thin pale-blue haze that
/// mostly hides the cut edges, and a neutral grade over whatever the donor
/// map tinted (mp_rust is desaturated sepia).
pub fn arena_fog() -> ExpFog {
    ExpFog {
        start_dist: 1500.0,
        halfway_dist: 36000.0,
        color_rgb: [0.74, 0.82, 0.92],
        max_opacity: 0.55,
        transition_time: 0.0,
        sun: None,
        volumetric: None,
    }
}

pub fn arena_vision(donor: Option<&FilmVision>) -> FilmVision {
    let glow = donor.filter(|v| v.glow_enable);
    FilmVision {
        enable: true,
        contrast: 1.06,
        brightness: 0.0,
        desaturation: 0.0,
        desaturation_dark: 0.0,
        invert: false,
        light_tint: [1.0, 1.0, 1.02],
        medium_tint: [1.0, 1.0, 1.0],
        dark_tint: [0.98, 1.0, 1.04],
        glow_enable: glow.is_some(),
        glow_radius: glow.map_or(0.0, |v| v.glow_radius),
        glow_bloom_cutoff: glow.map_or(0.0, |v| v.glow_bloom_cutoff),
        glow_bloom_desaturation: 0.0,
        glow_bloom_intensity: glow.map_or(0.0, |v| v.glow_bloom_intensity),
    }
}
