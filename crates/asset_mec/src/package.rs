use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::{gltf_dir_to_iw4, gltf_to_iw4};

/// One triangle list sharing a material, in IW4 space (inches, Z up), wound
/// counter-clockwise seen from the front as glTF authors it.
#[derive(Clone, Debug, Default)]
pub struct ArenaMesh {
    pub material: usize,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// Baked per-vertex light: rgb sky visibility, a sun visibility; empty when unbaked.
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

/// An arena material: base colour (factor multiplied into decoded PNG or
/// flat colour; block-compressed DDS colour maps carry a white factor) plus
/// optional IW4-ready normal and specular maps.
#[derive(Clone, Debug)]
pub struct ArenaTexture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// RGBA8 top level; empty when `compressed` holds the colour map.
    pub rgba: Vec<u8>,
    pub compressed: Option<Arc<ArenaImage>>,
    /// IW4 DXT5nm normal map (x in alpha, y in green).
    pub normal: Option<Arc<ArenaImage>>,
    /// IW4 specular map (rgb specular colour, a gloss), from the glTF
    /// metallicRoughnessTexture slot.
    pub specular: Option<Arc<ArenaImage>>,
    /// glTF `alphaMode: MASK`: draw alpha-tested, double sided.
    pub alpha_test: bool,
    /// glTF `alphaMode: BLEND`: glass and decals, drawn after the opaque world.
    pub blend: bool,
}

/// A block-compressed image with its whole mip chain, as stored in the DDS.
#[derive(Debug)]
pub struct ArenaImage {
    pub width: u32,
    pub height: u32,
    pub kind: BlockKind,
    pub levels: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Bc1,
    Bc3,
    Bc7,
}

impl BlockKind {
    pub fn block_bytes(self) -> usize {
        match self {
            Self::Bc1 => 8,
            Self::Bc3 | Self::Bc7 => 16,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ArenaSpawn {
    pub origin: [f32; 3],
    pub yaw: f32,
}

#[derive(Clone, Debug)]
pub struct ArenaPackage {
    pub name: String,
    pub dir: PathBuf,
    pub meshes: Vec<ArenaMesh>,
    pub textures: Vec<ArenaTexture>,
    /// Triangle soup for player and bullet collision, IW4 space.
    pub collision: Vec<[[f32; 3]; 3]>,
    pub spawns: Vec<ArenaSpawn>,
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    /// The playable volume (IW4 space); leaving it kills.
    pub play_mins: [f32; 3],
    pub play_maxs: [f32; 3],
    /// Direction the sunlight travels, IW4 space, normalized.
    pub sun_dir: [f32; 3],
    pub report: Vec<String>,
}

#[derive(Deserialize)]
struct ArenaJson {
    name: Option<String>,
    #[serde(default)]
    spawns: Vec<SpawnJson>,
    bounds_min: Option<[f32; 3]>,
    bounds_max: Option<[f32; 3]>,
    play_min: Option<[f32; 3]>,
    play_max: Option<[f32; 3]>,
    sun_dir: Option<[f32; 3]>,
}

#[derive(Deserialize)]
struct SpawnJson {
    origin: [f32; 3],
    #[serde(default)]
    yaw_deg: f32,
}

impl ArenaPackage {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let meta_path = dir.join("arena.json");
        let meta: ArenaJson = serde_json::from_slice(
            &std::fs::read(&meta_path).map_err(|e| format!("{}: {e}", meta_path.display()))?,
        )
        .map_err(|e| format!("{}: {e}", meta_path.display()))?;
        let mut report = Vec::new();

        let world = read_glb(&dir.join("arena.glb"), true)?;
        let collision_path = dir.join("collision.glb");
        let collision = if collision_path.is_file() {
            report.push("collision: collision.glb".to_owned());
            read_glb(&collision_path, false)?.triangles()
        } else {
            report.push("collision: arena.glb render triangles (no collision.glb)".to_owned());
            world.triangles()
        };
        if collision.is_empty() {
            return Err(format!("{}: no collision triangles", dir.display()));
        }

        let mut mins = [f32::INFINITY; 3];
        let mut maxs = [f32::NEG_INFINITY; 3];
        for p in world.meshes.iter().flat_map(|m| m.positions.iter()) {
            for a in 0..3 {
                mins[a] = mins[a].min(p[a]);
                maxs[a] = maxs[a].max(p[a]);
            }
        }
        if let (Some(lo), Some(hi)) = (meta.bounds_min, meta.bounds_max) {
            let (a, b) = (gltf_to_iw4(lo), gltf_to_iw4(hi));
            for k in 0..3 {
                mins[k] = mins[k].min(a[k].min(b[k]));
                maxs[k] = maxs[k].max(a[k].max(b[k]));
            }
        }
        if !mins[0].is_finite() {
            return Err(format!("{}: arena.glb has no triangles", dir.display()));
        }

        let spawns: Vec<ArenaSpawn> = meta
            .spawns
            .iter()
            .map(|s| ArenaSpawn {
                origin: gltf_to_iw4(s.origin),
                yaw: s.yaw_deg,
            })
            .collect();
        if spawns.is_empty() {
            return Err(format!("{}: arena.json lists no spawns", dir.display()));
        }
        let sun_dir = normalize(gltf_dir_to_iw4(meta.sun_dir.unwrap_or([-0.4, -0.8, -0.3])));
        let (play_mins, play_maxs) = match (meta.play_min, meta.play_max) {
            (Some(lo), Some(hi)) => {
                let (a, b) = (gltf_to_iw4(lo), gltf_to_iw4(hi));
                (
                    [0, 1, 2].map(|k| a[k].min(b[k])),
                    [0, 1, 2].map(|k| a[k].max(b[k])),
                )
            }
            // Older packages: the render bounds, with a floor under them.
            _ => (
                [mins[0], mins[1], mins[2] - 256.0],
                [maxs[0], maxs[1], maxs[2] + 1024.0],
            ),
        };
        let name = meta.name.unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "arena".to_owned())
        });
        report.push(format!(
            "arena `{name}`: {} meshes, {} render tris, {} textures, {} collision tris, {} spawns, bounds {:?}..{:?}",
            world.meshes.len(),
            world.meshes.iter().map(|m| m.indices.len() / 3).sum::<usize>(),
            world.textures.len(),
            collision.len(),
            spawns.len(),
            mins.map(|v| v.round()),
            maxs.map(|v| v.round()),
        ));
        report.extend(world.report);
        Ok(Self {
            name,
            dir: dir.to_path_buf(),
            meshes: world.meshes,
            textures: world.textures,
            collision,
            spawns,
            mins,
            maxs,
            play_mins,
            play_maxs,
            sun_dir,
            report,
        })
    }
}

struct GlbScene {
    meshes: Vec<ArenaMesh>,
    textures: Vec<ArenaTexture>,
    report: Vec<String>,
}

impl GlbScene {
    fn triangles(&self) -> Vec<[[f32; 3]; 3]> {
        let mut out = Vec::new();
        for mesh in &self.meshes {
            for tri in mesh.indices.chunks_exact(3) {
                let [a, b, c] = [0, 1, 2].map(|k| mesh.positions[tri[k] as usize]);
                let n = cross(sub(b, a), sub(c, a));
                if dot(n, n) > 1e-6 {
                    out.push([a, b, c]);
                }
            }
        }
        out
    }
}

fn read_glb(path: &Path, with_materials: bool) -> Result<GlbScene, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let gltf = gltf::Gltf::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new("."));
    let mut buffers = Vec::new();
    for buffer in gltf.document.buffers() {
        let data = match buffer.source() {
            gltf::buffer::Source::Bin => gltf
                .blob
                .clone()
                .ok_or_else(|| format!("{}: GLB has no BIN chunk", path.display()))?,
            gltf::buffer::Source::Uri(uri) => std::fs::read(base.join(uri))
                .map_err(|e| format!("{}: buffer `{uri}`: {e}", path.display()))?,
        };
        buffers.push(data);
    }

    let mut report = Vec::new();
    let mut textures = Vec::new();
    // Parsed DDS images by glTF texture index (shared between materials).
    let mut dds_cache: HashMap<usize, Option<Arc<ArenaImage>>> = HashMap::new();
    let mut dds_for = |texture: gltf::Texture<'_>, report: &mut Vec<String>| -> Option<Arc<ArenaImage>> {
        dds_cache
            .entry(texture.index())
            .or_insert_with(|| {
                let bytes = image_bytes(&texture.source(), &buffers, base)?;
                match parse_dds(&bytes) {
                    Ok(image) => Some(Arc::new(image)),
                    Err(error) => {
                        report.push(format!("texture {}: {error}", texture.index()));
                        None
                    }
                }
            })
            .clone()
    };
    // glTF material index (or none) -> arena texture index.
    let mut material_slots: Vec<Option<usize>> = vec![None; gltf.document.materials().len()];
    let mut default_slot = None;
    let mut slot_for = |material: gltf::Material<'_>, textures: &mut Vec<ArenaTexture>| -> usize {
        let Some(index) = material.index() else {
            return *default_slot.get_or_insert_with(|| {
                textures.push(solid("default", [0.7, 0.7, 0.7, 1.0]));
                textures.len() - 1
            });
        };
        if let Some(slot) = material_slots[index] {
            return slot;
        }
        let pbr = material.pbr_metallic_roughness();
        let factor = pbr.base_color_factor();
        let name = material
            .name()
            .map_or_else(|| format!("material{index}"), str::to_owned);
        let alpha_test = material.alpha_mode() == gltf::material::AlphaMode::Mask;
        let blend = material.alpha_mode() == gltf::material::AlphaMode::Blend;
        let normal = material
            .normal_texture()
            .and_then(|info| dds_for(info.texture(), &mut report));
        let specular = pbr
            .metallic_roughness_texture()
            .and_then(|info| dds_for(info.texture(), &mut report));
        let colour = pbr.base_color_texture().map(|info| info.texture());
        let compressed = colour
            .as_ref()
            .filter(|t| is_dds(&t.source()))
            .and_then(|t| dds_for(t.clone(), &mut report));
        let decoded = colour
            .filter(|t| !is_dds(&t.source()))
            .and_then(|t| image_bytes(&t.source(), &buffers, base))
            .and_then(|png| decode_png(&png).ok());
        let texture = match (compressed, decoded) {
            (Some(image), _) => ArenaTexture {
                name,
                width: image.width,
                height: image.height,
                rgba: Vec::new(),
                compressed: Some(image),
                normal,
                specular,
                alpha_test,
                blend,
            },
            (None, Some((width, height, mut rgba))) => {
                for px in rgba.chunks_exact_mut(4) {
                    for k in 0..4 {
                        px[k] = (f32::from(px[k]) * factor[k]).round().clamp(0.0, 255.0) as u8;
                    }
                }
                ArenaTexture {
                    name,
                    width,
                    height,
                    rgba,
                    compressed: None,
                    normal,
                    specular,
                    alpha_test,
                    blend,
                }
            }
            (None, None) => {
                if pbr.base_color_texture().is_some() {
                    report.push(format!(
                        "material `{name}`: base colour texture not decodable; factor only"
                    ));
                }
                ArenaTexture {
                    normal,
                    specular,
                    blend,
                    ..solid(&name, factor)
                }
            }
        };
        textures.push(texture);
        material_slots[index] = Some(textures.len() - 1);
        textures.len() - 1
    };

    let mut meshes: Vec<ArenaMesh> = Vec::new();
    let scene = gltf
        .document
        .default_scene()
        .or_else(|| gltf.document.scenes().next())
        .ok_or_else(|| format!("{}: no scene", path.display()))?;
    let mut stack: Vec<(gltf::Node<'_>, Mat4)> =
        scene.nodes().map(|node| (node, Mat4::IDENTITY)).collect();
    let mut skipped_modes = 0usize;
    while let Some((node, parent)) = stack.pop() {
        let world = parent.mul(&Mat4(node.transform().matrix()));
        stack.extend(node.children().map(|child| (child, world)));
        let Some(mesh) = node.mesh() else {
            continue;
        };
        let flip = world.det3() < 0.0;
        let normal_matrix = world.normal_matrix();
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                skipped_modes += 1;
                continue;
            }
            let reader = primitive.reader(|b| Some(&buffers[b.index()][..]));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.map(|p| world.point(p)).collect();
            let indices: Vec<u32> = match reader.read_indices() {
                Some(indices) => indices.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };
            let mut normals: Vec<[f32; 3]> = match reader.read_normals() {
                Some(normals) => normals
                    .map(|n| normalize(mat3_mul(&normal_matrix, n)))
                    .collect(),
                None => Vec::new(),
            };
            if normals.len() != positions.len() {
                normals = vertex_normals(&positions, &indices);
            }
            let uvs: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
                Some(uvs) => uvs.into_f32().collect(),
                None => vec![[0.0; 2]; positions.len()],
            };
            let colors: Vec<[f32; 4]> = match reader.read_colors(0) {
                Some(c) => c.into_rgba_f32().collect(),
                None => Vec::new(),
            };
            // A short or long colour stream is ignored rather than misread.
            let colors = if colors.len() == positions.len() {
                colors
            } else {
                Vec::new()
            };
            let material = if with_materials {
                slot_for(primitive.material(), &mut textures)
            } else {
                0
            };
            let mut out = ArenaMesh {
                material,
                positions: positions.into_iter().map(gltf_to_iw4).collect(),
                normals: normals.into_iter().map(gltf_dir_to_iw4).collect(),
                uvs,
                colors,
                indices: Vec::with_capacity(indices.len()),
            };
            for tri in indices.chunks_exact(3) {
                if tri.iter().any(|&i| i as usize >= out.positions.len()) {
                    continue;
                }
                if flip {
                    out.indices.extend_from_slice(&[tri[0], tri[2], tri[1]]);
                } else {
                    out.indices.extend_from_slice(tri);
                }
            }
            meshes.push(out);
        }
    }
    if skipped_modes != 0 {
        report.push(format!(
            "{}: {skipped_modes} non-triangle primitives skipped",
            path.display()
        ));
    }
    Ok(GlbScene {
        meshes,
        textures,
        report,
    })
}

fn solid(name: &str, factor: [f32; 4]) -> ArenaTexture {
    let px = factor.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8);
    ArenaTexture {
        name: name.to_owned(),
        width: 4,
        height: 4,
        rgba: px.repeat(16),
        compressed: None,
        normal: None,
        specular: None,
        alpha_test: false,
        blend: false,
    }
}

const DDS_MIME: &str = "image/vnd-ms.dds";

fn is_dds(image: &gltf::Image<'_>) -> bool {
    match image.source() {
        gltf::image::Source::View { mime_type, .. } => mime_type == DDS_MIME,
        gltf::image::Source::Uri { uri, mime_type } => {
            mime_type == Some(DDS_MIME) || uri.to_ascii_lowercase().ends_with(".dds")
        }
    }
}

fn image_bytes(image: &gltf::Image<'_>, buffers: &[Vec<u8>], base: &Path) -> Option<Vec<u8>> {
    match image.source() {
        gltf::image::Source::View { view, .. } => buffers[view.buffer().index()]
            .get(view.offset()..view.offset() + view.length())
            .map(<[u8]>::to_vec),
        gltf::image::Source::Uri { uri, .. } => std::fs::read(base.join(uri)).ok(),
    }
}

/// DDS with a DX10 header (BC1/BC3/BC7, sRGB or not: the material decides how
/// the bytes are read). Levels past the end of the data are dropped.
pub(crate) fn parse_dds(bytes: &[u8]) -> Result<ArenaImage, String> {
    let u32_at = |o: usize| -> Result<u32, String> {
        bytes
            .get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| "truncated DDS header".to_owned())
    };
    if bytes.get(..4) != Some(b"DDS ".as_slice()) {
        return Err("not a DDS file".into());
    }
    let (height, width) = (u32_at(12)?, u32_at(16)?);
    let mips = u32_at(28)?.max(1);
    if bytes.get(84..88) != Some(b"DX10".as_slice()) {
        return Err("DDS without a DX10 header".into());
    }
    let kind = match u32_at(128)? {
        70..=72 => BlockKind::Bc1,
        76..=78 => BlockKind::Bc3,
        97..=99 => BlockKind::Bc7,
        other => return Err(format!("unsupported DXGI format {other}")),
    };
    if width == 0 || height == 0 || width % 4 != 0 || height % 4 != 0 {
        return Err(format!("{width}x{height} is not a multiple of the 4x4 block"));
    }
    let data = &bytes[148.min(bytes.len())..];
    let (mut used, mut levels, mut w, mut h) = (0usize, 0u32, width, height);
    while levels < mips {
        let size = (w.div_ceil(4) * h.div_ceil(4)) as usize * kind.block_bytes();
        if used + size > data.len() {
            break;
        }
        used += size;
        levels += 1;
        if w == 1 && h == 1 {
            break;
        }
        (w, h) = ((w / 2).max(1), (h / 2).max(1));
    }
    if levels == 0 {
        return Err("DDS has no complete level".into());
    }
    Ok(ArenaImage {
        width,
        height,
        kind,
        levels,
        data: data[..used].to_vec(),
    })
}

fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("png too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|ga| [ga[0], ga[0], ga[0], ga[1]])
            .collect(),
        other => return Err(format!("unexpected png colour type {other:?}")),
    };
    Ok((info.width, info.height, rgba))
}

fn vertex_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![[0.0; 3]; positions.len()];
    for tri in indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|k| tri[k] as usize);
        if a.max(b).max(c) >= positions.len() {
            continue;
        }
        let n = cross(
            sub(positions[b], positions[a]),
            sub(positions[c], positions[a]),
        );
        for v in [a, b, c] {
            for k in 0..3 {
                normals[v][k] += n[k];
            }
        }
    }
    normals.into_iter().map(normalize).collect()
}

/// Column-major, as glTF stores it.
#[derive(Clone, Copy)]
struct Mat4([[f32; 4]; 4]);

impl Mat4 {
    const IDENTITY: Self = Self([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    fn mul(&self, rhs: &Self) -> Self {
        let mut out = [[0.0; 4]; 4];
        for (c, column) in out.iter_mut().enumerate() {
            for (r, value) in column.iter_mut().enumerate() {
                *value = (0..4).map(|k| self.0[k][r] * rhs.0[c][k]).sum();
            }
        }
        Self(out)
    }

    fn point(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.0;
        [0, 1, 2].map(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r])
    }

    fn det3(&self) -> f32 {
        let m = &self.0;
        dot(
            m[0][..3].try_into().unwrap_or([0.0; 3]),
            cross(
                m[1][..3].try_into().unwrap_or([0.0; 3]),
                m[2][..3].try_into().unwrap_or([0.0; 3]),
            ),
        )
    }

    /// Rows of the inverse transpose of the upper 3x3 (cofactor matrix; the
    /// scale is dropped by the normalize that follows).
    fn normal_matrix(&self) -> [[f32; 3]; 3] {
        let c = |i: usize| -> [f32; 3] { [self.0[i][0], self.0[i][1], self.0[i][2]] };
        let (c0, c1, c2) = (c(0), c(1), c(2));
        let sign = if self.det3() < 0.0 { -1.0 } else { 1.0 };
        let rows = [cross(c1, c2), cross(c2, c0), cross(c0, c1)];
        // cofactor rows indexed by column of the original -> transpose into output rows
        let mut out = [[0.0; 3]; 3];
        for (j, row) in rows.iter().enumerate() {
            for i in 0..3 {
                out[i][j] = row[i] * sign;
            }
        }
        out
    }
}

fn mat3_mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

pub(crate) fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt();
    if len > 1e-8 {
        v.map(|c| c / len)
    } else {
        [0.0, 0.0, 1.0]
    }
}
