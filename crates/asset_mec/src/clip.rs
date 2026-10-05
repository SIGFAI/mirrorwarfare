use std::sync::Arc;

use asset_world::{ClipCmodel, ClipCollision, ClipMapMaterial};
use clipmap_iw4::{ClipAabbNode, ClipMeshTables, ClipPartition, VERTS_PER_SEGMENT};

use crate::ArenaPackage;
use crate::package::{cross, dot, sub};

const CONTENTS_SOLID: u32 = 1;
/// Surface type 5 (concrete) in bits 20..25: impacts, footsteps, penetration.
const SURF_CONCRETE: u32 = 5 << 20;
const LEAF_TRIS: usize = 24;
const WALKABLE_NORMAL_Z: f32 = 0.7;

/// The arena's collision triangles as clipmap mesh tables: a bounding-volume
/// tree over partitions of at most `LEAF_TRIS` triangles, no brushes and no
/// BSP, so every trace walks the whole AABB forest from its single root.
pub fn build_clip_collision(arena: &ArenaPackage) -> Result<ClipCollision, String> {
    // IW4 collision triangles face the side their (v0 - v2) x (v0 - v1) normal
    // points to, i.e. clockwise seen from the front: reverse the glTF winding.
    let tris: Vec<[[f32; 3]; 3]> = arena.collision.iter().map(|t| [t[0], t[2], t[1]]).collect();
    let mut order: Vec<usize> = (0..tris.len()).collect();
    let centroids: Vec<[f32; 3]> = tris
        .iter()
        .map(|t| [0, 1, 2].map(|a| (t[0][a] + t[1][a] + t[2][a]) / 3.0))
        .collect();

    let mut mesh = ClipMeshTables::default();
    let mut nodes: Vec<ClipAabbNode> = vec![ClipAabbNode::default()];
    build_node(&tris, &centroids, &mut order[..], 0, &mut nodes, &mut mesh)?;
    mesh.aabb_trees = nodes;
    mesh.aabb_roots = vec![0];

    let tri_count = mesh.tri_indices.len() / 3;
    mesh.tri_content_flags = vec![CONTENTS_SOLID; tri_count];
    mesh.tri_surface_flags = vec![SURF_CONCRETE; tri_count];
    // Every edge of an upward-facing triangle is walkable; IW4 bakes this per
    // edge so a capsule may slide across shared edges of a floor.
    let mut walkable = vec![0u8; (tri_count * 3).div_ceil(8)];
    for part in &mesh.partitions {
        let base = part.first_vert_segment as usize * VERTS_PER_SEGMENT;
        for ti in part.first_tri as usize..part.first_tri as usize + part.tri_count as usize {
            let [a, b, c] =
                [0, 1, 2].map(|k| mesh.verts[base + mesh.tri_indices[ti * 3 + k] as usize]);
            let n = cross(sub(a, c), sub(a, b));
            let len = dot(n, n).sqrt();
            if len > 0.0 && n[2] / len >= WALKABLE_NORMAL_Z {
                for k in 0..3 {
                    let bit = ti * 3 + k;
                    walkable[bit >> 3] |= 1 << (bit & 7);
                }
            }
        }
    }
    mesh.tri_edge_is_walkable = walkable;

    Ok(ClipCollision {
        mesh: Arc::new(mesh),
        tri_material_index: vec![0; tri_count],
        materials: vec![ClipMapMaterial {
            name: "mec/collision".to_owned(),
            surface_flags: SURF_CONCRETE,
            content_flags: CONTENTS_SOLID,
        }],
        cmodels: vec![ClipCmodel {
            mins: arena.mins,
            maxs: arena.maxs,
            radius: radius(arena.mins, arena.maxs),
            first_brush: 0,
            num_brushes: 0,
        }],
        ..ClipCollision::default()
    })
}

fn radius(mins: [f32; 3], maxs: [f32; 3]) -> f32 {
    let half = [0, 1, 2].map(|a| (maxs[a] - mins[a]) * 0.5);
    dot(half, half).sqrt()
}

fn bounds_of(tris: &[[[f32; 3]; 3]], order: &[usize]) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for &t in order {
        for v in &tris[t] {
            for a in 0..3 {
                lo[a] = lo[a].min(v[a]);
                hi[a] = hi[a].max(v[a]);
            }
        }
    }
    (lo, hi)
}

fn build_node(
    tris: &[[[f32; 3]; 3]],
    centroids: &[[f32; 3]],
    order: &mut [usize],
    slot: usize,
    nodes: &mut Vec<ClipAabbNode>,
    mesh: &mut ClipMeshTables,
) -> Result<(), String> {
    let (lo, hi) = bounds_of(tris, order);
    let origin = [0, 1, 2].map(|a| (lo[a] + hi[a]) * 0.5);
    let half_size = [0, 1, 2].map(|a| (hi[a] - lo[a]) * 0.5 + 1.0);
    if order.len() <= LEAF_TRIS {
        let partition = emit_partition(tris, order, mesh)?;
        nodes[slot] = ClipAabbNode {
            origin,
            half_size,
            material_index: 0,
            child_count: 0,
            u: partition as i32,
        };
        return Ok(());
    }
    let axis = (0..3)
        .max_by(|&a, &b| (hi[a] - lo[a]).total_cmp(&(hi[b] - lo[b])))
        .unwrap_or(0);
    order.sort_unstable_by(|&a, &b| centroids[a][axis].total_cmp(&centroids[b][axis]));
    let first = nodes.len();
    nodes.push(ClipAabbNode::default());
    nodes.push(ClipAabbNode::default());
    nodes[slot] = ClipAabbNode {
        origin,
        half_size,
        material_index: 0,
        child_count: 2,
        u: first as i32,
    };
    let mid = order.len() / 2;
    let (left, right) = order.split_at_mut(mid);
    build_node(tris, centroids, left, first, nodes, mesh)?;
    build_node(tris, centroids, right, first + 1, nodes, mesh)
}

fn emit_partition(
    tris: &[[[f32; 3]; 3]],
    order: &[usize],
    mesh: &mut ClipMeshTables,
) -> Result<usize, String> {
    let segment = mesh.verts.len() / VERTS_PER_SEGMENT;
    let segment = u8::try_from(segment).map_err(|_| {
        "arena collision exceeds 256 vertex segments; simplify collision.glb".to_owned()
    })?;
    let base = segment as usize * VERTS_PER_SEGMENT;
    let first_tri = mesh.tri_indices.len() / 3;
    let mut local: Vec<[f32; 3]> = Vec::new();
    for &t in order {
        for v in tris[t] {
            let index = match local.iter().position(|p| *p == v) {
                Some(i) => i,
                None => {
                    local.push(v);
                    mesh.verts.push(v);
                    local.len() - 1
                }
            };
            let global = mesh.verts.len() - local.len() + index;
            mesh.tri_indices.push((global - base) as u16);
        }
    }
    mesh.partitions.push(ClipPartition {
        tri_count: order.len() as u8,
        first_tri: first_tri as i32,
        first_vert_segment: segment,
        border_count: 0,
        first_border: 0,
    });
    Ok(mesh.partitions.len() - 1)
}
