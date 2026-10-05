//! Faith's third-person parkour clips from Mirror's Edge Catalyst, played on IW4 bodies.
//!
//! The clip pack is written by `tools/mec/antanim.py export` (run from `tools/mec/setup.ps1`)
//! into a `mec-anims` folder outside the repo (game data is never committed):
//!
//! * `skeleton.json` — Faith's body joints (name, parent, bind local rotation / translation),
//!   already in IW4 axes (x forward, y left, z up) and inches;
//! * `clips/<name>.json` — per frame local rotations of those joints and the `Hips`
//!   translation relative to the (pinned) trajectory joint, i.e. root motion stripped;
//!   `seconds` is the ANT clip length (`NumTicks / 60`).
//!
//! Folder precedence (like the arenas): `IW4L_MEC_ANIMS`, `<artifacts>/mec-anims`, then the
//! first `mec-anims` beside the executable or any parent, then the same walk from the
//! working directory.
//!
//! Retargeting happens against the live DObj (any MW2 body model): per mapped bone the
//! Catalyst model-space rotation drives the IW4 bone's model-space rotation through a fixed
//! offset that aligns the two bind poses bone by bone (bone direction + a global reference
//! axis), so rest-pose differences (Faith's A-pose vs the MW2 bind) cancel out. The pelvis
//! position is Faith's hips position scaled by the hip-height ratio. Locals are written
//! back relative to the actually composed parent, so unmapped in-between bones (IW4
//! `pelvis` / `back_*` controller tags, twist bones) keep working.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use glam::{Mat3, Quat, Vec3};
use serde::Deserialize;
use xmodel_runtime::DObj;

pub const ANIMS_ENV: &str = "IW4L_MEC_ANIMS";
const ANIMS_DIR: &str = "mec-anims";

#[derive(Deserialize)]
struct SkeletonFile {
    joints: Vec<JointFile>,
}

#[derive(Deserialize)]
struct JointFile {
    name: String,
    parent: i32,
    bind_q: [f32; 4],
    bind_t: [f32; 3],
}

#[derive(Deserialize)]
struct ClipFile {
    name: String,
    seconds: f32,
    frames: usize,
    #[serde(default)]
    distance_m: f32,
    q: Vec<Vec<[f32; 4]>>,
    hips_t: Vec<[f32; 3]>,
}

/// Faith's body skeleton (pack joint order).
#[derive(Debug, Clone)]
pub struct SrcSkeleton {
    pub names: Vec<String>,
    pub parents: Vec<Option<usize>>,
    pub bind_q: Vec<Quat>,
    pub bind_t: Vec<Vec3>,
    pub bind_world_q: Vec<Quat>,
    pub bind_world_p: Vec<Vec3>,
    pub traj: usize,
    pub hips: usize,
}

impl SrcSkeleton {
    pub fn find(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// Model-space rotations and positions of a pose (trajectory joint pinned at bind).
    pub fn world(&self, pose: &SrcPose, wq: &mut Vec<Quat>, wp: &mut Vec<Vec3>) {
        let n = self.names.len();
        wq.clear();
        wp.clear();
        for i in 0..n {
            let lq = if i == self.traj {
                self.bind_q[i]
            } else {
                pose.q[i]
            };
            let lt = if i == self.hips {
                pose.hips
            } else {
                self.bind_t[i]
            };
            match self.parents[i] {
                Some(p) => {
                    let pq = wq[p];
                    let pp = wp[p];
                    wq.push((pq * lq).normalize());
                    wp.push(pp + pq * lt);
                }
                None => {
                    wq.push(lq);
                    wp.push(lt);
                }
            }
        }
    }
}

/// One Catalyst clip, frames uniformly spread over `seconds`.
#[derive(Debug, Clone)]
pub struct Clip {
    pub name: String,
    pub seconds: f32,
    pub frames: usize,
    /// Root distance of the ANT clip (m), for stride matching of loops.
    pub distance_m: f32,
    joints: usize,
    q: Vec<Quat>,
    hips: Vec<Vec3>,
}

/// Local pose of the source skeleton.
#[derive(Debug, Clone, Default)]
pub struct SrcPose {
    pub q: Vec<Quat>,
    pub hips: Vec3,
}

impl Clip {
    /// Sample at `u` in 0..=1 of the clip (linear between frames, nlerp rotations).
    pub fn sample(&self, u: f32, out: &mut SrcPose) {
        let f = u.clamp(0.0, 1.0) * (self.frames.saturating_sub(1)) as f32;
        let a = (f.floor() as usize).min(self.frames - 1);
        let b = (a + 1).min(self.frames - 1);
        let w = f - a as f32;
        out.q.clear();
        for j in 0..self.joints {
            let qa = self.q[a * self.joints + j];
            let mut qb = self.q[b * self.joints + j];
            if qa.dot(qb) < 0.0 {
                qb = -qb;
            }
            out.q.push(qa.lerp(qb, w).normalize());
        }
        out.hips = self.hips[a].lerp(self.hips[b], w);
    }
}

/// `out = a` blended toward `b` by `w`.
pub fn blend_pose(a: &SrcPose, b: &SrcPose, w: f32, out: &mut SrcPose) {
    out.q.clear();
    for (qa, qb) in a.q.iter().zip(&b.q) {
        let qb = if qa.dot(*qb) < 0.0 { -*qb } else { *qb };
        out.q.push(qa.lerp(qb, w).normalize());
    }
    out.hips = a.hips.lerp(b.hips, w);
}

#[derive(Debug, Clone)]
pub struct Pack {
    pub dir: PathBuf,
    pub skeleton: SrcSkeleton,
    pub clips: Vec<Clip>,
    by_name: HashMap<String, usize>,
}

impl Pack {
    pub fn clip(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    pub fn load(dir: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(dir.join("skeleton.json"))
            .map_err(|e| format!("{}: {e}", dir.join("skeleton.json").display()))?;
        let sk: SkeletonFile =
            serde_json::from_str(&text).map_err(|e| format!("skeleton.json: {e}"))?;
        let names: Vec<String> = sk.joints.iter().map(|j| j.name.clone()).collect();
        let parents: Vec<Option<usize>> = sk
            .joints
            .iter()
            .map(|j| usize::try_from(j.parent).ok())
            .collect();
        let bind_q: Vec<Quat> = sk
            .joints
            .iter()
            .map(|j| Quat::from_array(j.bind_q).normalize())
            .collect();
        let bind_t: Vec<Vec3> = sk
            .joints
            .iter()
            .map(|j| Vec3::from_array(j.bind_t))
            .collect();
        let find = |n: &str| names.iter().position(|x| x == n);
        let traj = find("AITrajectory").ok_or("skeleton has no AITrajectory")?;
        let hips = find("Hips").ok_or("skeleton has no Hips")?;
        if parents
            .iter()
            .enumerate()
            .any(|(i, p)| p.is_some_and(|p| p >= i))
        {
            return Err("skeleton joints are not parent-first".into());
        }
        let mut skeleton = SrcSkeleton {
            names,
            parents,
            bind_q,
            bind_t,
            bind_world_q: Vec::new(),
            bind_world_p: Vec::new(),
            traj,
            hips,
        };
        let bind_pose = SrcPose {
            q: skeleton.bind_q.clone(),
            hips: skeleton.bind_t[hips],
        };
        let (mut wq, mut wp) = (Vec::new(), Vec::new());
        skeleton.world(&bind_pose, &mut wq, &mut wp);
        skeleton.bind_world_q = wq;
        skeleton.bind_world_p = wp;

        let mut clips = Vec::new();
        let mut by_name = HashMap::new();
        let clip_dir = dir.join("clips");
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&clip_dir)
            .map_err(|e| format!("{}: {e}", clip_dir.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        entries.sort();
        let joints = skeleton.names.len();
        for path in entries {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let c: ClipFile =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if c.frames == 0
                || c.q.len() != c.frames
                || c.hips_t.len() != c.frames
                || c.q.iter().any(|f| f.len() != joints)
            {
                return Err(format!(
                    "{}: frame data does not match the skeleton",
                    path.display()
                ));
            }
            let q =
                c.q.iter()
                    .flat_map(|f| f.iter().map(|q| Quat::from_array(*q).normalize()))
                    .collect();
            let hips = c.hips_t.iter().map(|t| Vec3::from_array(*t)).collect();
            by_name.insert(c.name.clone(), clips.len());
            clips.push(Clip {
                name: c.name,
                seconds: c.seconds.max(1.0 / 30.0),
                frames: c.frames,
                distance_m: c.distance_m,
                joints,
                q,
                hips,
            });
        }
        if clips.is_empty() {
            return Err(format!("{}: no clips", clip_dir.display()));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            skeleton,
            clips,
            by_name,
        })
    }

    /// The pack found by [`anims_root`], loaded once per process (`None` when absent or
    /// broken; the reason is in [`global_status`]).
    pub fn global() -> Option<&'static Pack> {
        global_cell().as_ref().ok()
    }
}

fn global_cell() -> &'static Result<Pack, String> {
    static PACK: OnceLock<Result<Pack, String>> = OnceLock::new();
    PACK.get_or_init(|| {
        if std::env::var("IW4L_MEC_ANIMS_OFF").is_ok_and(|v| v == "1") {
            return Err("disabled by IW4L_MEC_ANIMS_OFF=1".into());
        }
        let dir = anims_root();
        Pack::load(&dir)
    })
}

/// One line for the log: where the pack came from or why there is none.
pub fn global_status() -> String {
    match global_cell() {
        Ok(p) => format!("{} clips from {}", p.clips.len(), p.dir.display()),
        Err(e) => format!("no Catalyst clip pack ({e}); stock MW2 clips are used"),
    }
}

/// `IW4L_MEC_ANIMS`, else `<artifacts>/mec-anims`, else the first `mec-anims` beside the
/// executable or any of its parents, then the same walk from the working directory.
pub fn anims_root() -> PathBuf {
    if let Some(dir) = std::env::var_os(ANIMS_ENV).filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let artifacts = std::env::var_os("IW4L_ARTIFACTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("iw4l-artifacts"))
        .join(ANIMS_DIR);
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
                .map(|d| d.join(ANIMS_DIR))
                .collect::<Vec<_>>()
        })
        .find(|d| d.join("skeleton.json").is_file())
        .unwrap_or(artifacts)
}

// ------------------------------------------------------------------ retarget

/// Which body part a mapped bone belongs to (callers weight parts separately, e.g. the
/// weapon arms stay on the MW2 animation while the legs play Catalyst).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Root,
    Spine,
    Head,
    Arm,
    Leg,
}

/// (Faith joint, IW4 bone, Faith child for the bone direction, IW4 child, part).
/// A missing child means "use the parent's direction" (hands, toes, head).
const BONE_MAP: &[(&str, &str, &str, &str, Part)] = &[
    ("Hips", "j_mainroot", "Spine2", "j_spine4", Part::Root),
    (
        "Spine",
        "j_spinelower",
        "Spine1",
        "j_spineupper",
        Part::Spine,
    ),
    ("Spine1", "j_spineupper", "Spine2", "j_spine4", Part::Spine),
    ("Spine2", "j_spine4", "Neck", "j_neck", Part::Spine),
    ("Neck", "j_neck", "Head", "j_head", Part::Head),
    ("Head", "j_head", "", "", Part::Head),
    (
        "LeftShoulder",
        "j_clavicle_le",
        "LeftArm",
        "j_shoulder_le",
        Part::Arm,
    ),
    (
        "LeftArm",
        "j_shoulder_le",
        "LeftForeArm",
        "j_elbow_le",
        Part::Arm,
    ),
    (
        "LeftForeArm",
        "j_elbow_le",
        "LeftHand",
        "j_wrist_le",
        Part::Arm,
    ),
    ("LeftHand", "j_wrist_le", "", "", Part::Arm),
    (
        "RightShoulder",
        "j_clavicle_ri",
        "RightArm",
        "j_shoulder_ri",
        Part::Arm,
    ),
    (
        "RightArm",
        "j_shoulder_ri",
        "RightForeArm",
        "j_elbow_ri",
        Part::Arm,
    ),
    (
        "RightForeArm",
        "j_elbow_ri",
        "RightHand",
        "j_wrist_ri",
        Part::Arm,
    ),
    ("RightHand", "j_wrist_ri", "", "", Part::Arm),
    ("LeftUpLeg", "j_hip_le", "LeftLeg", "j_knee_le", Part::Leg),
    ("LeftLeg", "j_knee_le", "LeftFoot", "j_ankle_le", Part::Leg),
    (
        "LeftFoot",
        "j_ankle_le",
        "LeftToeBase",
        "j_ball_le",
        Part::Leg,
    ),
    ("LeftToeBase", "j_ball_le", "", "", Part::Leg),
    ("RightUpLeg", "j_hip_ri", "RightLeg", "j_knee_ri", Part::Leg),
    (
        "RightLeg",
        "j_knee_ri",
        "RightFoot",
        "j_ankle_ri",
        Part::Leg,
    ),
    (
        "RightFoot",
        "j_ankle_ri",
        "RightToeBase",
        "j_ball_ri",
        Part::Leg,
    ),
    ("RightToeBase", "j_ball_ri", "", "", Part::Leg),
];

#[derive(Debug, Clone, Copy)]
struct Mapped {
    src: usize,
    /// target model rotation = source model rotation * k
    k: Quat,
    part: Part,
    root: bool,
}

/// Faith → one DObj skeleton.
#[derive(Debug, Clone)]
pub struct Retarget {
    by_dst: Vec<Option<Mapped>>,
    /// hips position scale (dst pelvis height / Faith hips height)
    pub scale: f32,
    pub mapped: usize,
}

/// Bind model-space rotation / position of every DObj bone (the same composition as
/// `DObj::compose` with bind locals).
fn dobj_bind_world(dobj: &DObj) -> (Vec<Quat>, Vec<Vec3>) {
    let n = dobj.bones.len();
    let (mut wq, mut wp) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for b in &dobj.bones {
        let (pq, pp) = b
            .parent
            .map_or((Quat::IDENTITY, Vec3::ZERO), |p| (wq[p], wp[p]));
        wq.push((pq * b.bind_rotation).normalize());
        wp.push(pp + pq * b.bind_translation);
    }
    (wq, wp)
}

fn frame(primary: Vec3, reference: Vec3) -> Mat3 {
    let p = primary.normalize_or_zero();
    let s = (reference - p * reference.dot(p)).normalize_or_zero();
    Mat3::from_cols(p, s, p.cross(s))
}

impl Retarget {
    /// `None` when the DObj lacks the core bones (not a player body).
    pub fn new(skel: &SrcSkeleton, dobj: &DObj) -> Option<Self> {
        let (dq, dp) = dobj_bind_world(dobj);
        let mut by_dst = vec![None; dobj.bones.len()];
        let mut mapped = 0;
        // (src joint, dst bone, src dir, dst dir) of every resolved entry, for children
        // without their own direction.
        let mut dirs: HashMap<&str, (Vec3, Vec3)> = HashMap::new();
        let parent_of = |name: &str| -> &'static str {
            match name {
                "LeftHand" => "LeftForeArm",
                "RightHand" => "RightForeArm",
                "LeftToeBase" => "LeftFoot",
                "RightToeBase" => "RightFoot",
                "Head" => "Neck",
                _ => "",
            }
        };
        for &(src_name, dst_name, src_child, dst_child, part) in BONE_MAP {
            let (Some(s), Some(d)) = (skel.find(src_name), dobj.find(dst_name)) else {
                continue;
            };
            let child_dirs = match (skel.find(src_child), dobj.find(dst_child)) {
                (Some(sc), Some(dc)) => {
                    Some((skel.bind_world_p[sc] - skel.bind_world_p[s], dp[dc] - dp[d]))
                }
                _ => None,
            };
            let Some((ps, pd)) = child_dirs.or_else(|| dirs.get(parent_of(src_name)).copied())
            else {
                continue;
            };
            if ps.length_squared() < 1e-8 || pd.length_squared() < 1e-8 {
                continue;
            }
            dirs.insert(src_name, (ps, pd));
            let psn = ps.normalize();
            let reference = if psn.x.abs() < 0.7 { Vec3::X } else { Vec3::Z };
            let fs = frame(ps, reference);
            let fd = frame(pd, reference);
            let align = Quat::from_mat3(&(fs * fd.transpose())).normalize();
            let k = (skel.bind_world_q[s].inverse() * align * dq[d]).normalize();
            by_dst[d] = Some(Mapped {
                src: s,
                k,
                part,
                root: part == Part::Root,
            });
            mapped += 1;
        }
        let pelvis = dobj.find("j_mainroot")?;
        dobj.find("j_hip_le")?;
        dobj.find("j_spine4")?;
        let src_h = skel.bind_world_p[skel.hips].z;
        let dst_h = dp[pelvis].z;
        let scale = if src_h > 1.0 && dst_h > 1.0 {
            dst_h / src_h
        } else {
            1.0
        };
        Some(Self {
            by_dst,
            scale,
            mapped,
        })
    }

    /// Writes the retargeted pose into `locals` (before the IW4 player controller runs),
    /// blended over the current locals by `weight(part)`.
    pub fn apply(
        &self,
        skel: &SrcSkeleton,
        pose: &SrcPose,
        dobj: &DObj,
        locals: &mut [anim_iw4::Local],
        weight: impl Fn(Part) -> f32,
    ) {
        let (mut sq, mut sp) = (Vec::new(), Vec::new());
        skel.world(pose, &mut sq, &mut sp);
        let n = dobj.bones.len().min(locals.len());
        let mut wq: Vec<Quat> = Vec::with_capacity(n);
        let mut wp: Vec<Vec3> = Vec::with_capacity(n);
        for i in 0..n {
            let bone = &dobj.bones[i];
            let scale = if bone.no_scale {
                1.0
            } else {
                dobj.models.get(bone.model).map_or(1.0, |m| m.scale)
            };
            let (pq, pp) = bone
                .parent
                .filter(|&p| p < i)
                .map_or((Quat::IDENTITY, Vec3::ZERO), |p| (wq[p], wp[p]));
            let l = &mut locals[i];
            let mut lq = Quat::from_array(l.rotation).normalize();
            let mut lt = bone.bind_translation + Vec3::from_array(l.translation) * scale;
            if let Some(m) = self.by_dst.get(i).copied().flatten() {
                let w = weight(m.part).clamp(0.0, 1.0);
                if w > 0.0 {
                    let target = sq[m.src] * m.k;
                    let mut want = (pq.inverse() * target).normalize();
                    if want.dot(lq) < 0.0 {
                        want = -want;
                    }
                    lq = lq.lerp(want, w).normalize();
                    l.rotation = lq.to_array();
                    l.control = false;
                    if m.root {
                        let pos = sp[m.src] * self.scale;
                        let want_t = pq.inverse() * (pos - pp);
                        lt = lt.lerp(want_t, w);
                        let delta = (lt - bone.bind_translation) / scale.max(1e-6);
                        l.translation = delta.to_array();
                    }
                }
            }
            wq.push((pq * lq).normalize());
            wp.push(pp + pq * lt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Mat4;
    use xmodel_runtime::{Bone, ModelSlot};

    /// A DObj with Faith's own joints renamed to the IW4 names (optionally with the left arm
    /// swung 35 degrees about +X in the bind, an A-pose / T-pose style difference).
    fn faith_as_dobj(skel: &SrcSkeleton, swing_arm: bool) -> DObj {
        let rename: HashMap<&str, &str> = BONE_MAP.iter().map(|m| (m.0, m.1)).collect();
        let mut bones: Vec<Bone> = Vec::new();
        let mut wq: Vec<Quat> = Vec::new();
        for i in 0..skel.names.len() {
            let name = if i == skel.traj {
                "tag_origin"
            } else {
                skel.names[i].as_str()
            };
            let name = rename.get(name).copied().unwrap_or(name).to_string();
            let parent = skel.parents[i];
            let pq = parent.map_or(Quat::IDENTITY, |p| wq[p]);
            let mut lq = skel.bind_q[i];
            if swing_arm && name == "j_shoulder_le" {
                lq = pq.inverse() * Quat::from_rotation_x(35f32.to_radians()) * pq * lq;
            }
            let lt = if i == skel.traj {
                Vec3::ZERO
            } else {
                skel.bind_t[i]
            };
            wq.push(pq * lq);
            bones.push(Bone {
                name,
                model: 0,
                parent,
                bind_rotation: lq,
                bind_translation: lt,
                bind_world: Mat4::IDENTITY,
                no_scale: false,
            });
        }
        DObj {
            models: vec![ModelSlot {
                name: "faith".into(),
                base: 0,
                bone_count: bones.len(),
                scale: 1.0,
            }],
            bones,
            duplicates: Vec::new(),
        }
    }

    fn posed(dobj: &DObj, locals: &[anim_iw4::Local]) -> Vec<Vec3> {
        let mut wq: Vec<Quat> = Vec::new();
        let mut wp: Vec<Vec3> = Vec::new();
        for (b, l) in dobj.bones.iter().zip(locals) {
            let (pq, pp) = b
                .parent
                .map_or((Quat::IDENTITY, Vec3::ZERO), |p| (wq[p], wp[p]));
            let lq = Quat::from_array(l.rotation);
            let lt = b.bind_translation + Vec3::from_array(l.translation);
            wq.push(pq * lq);
            wp.push(pp + pq * lt);
        }
        wp
    }

    #[test]
    fn retarget_onto_own_skeleton_reproduces_positions() {
        let Ok(pack) = Pack::load(&anims_root()) else {
            eprintln!("no clip pack; skipped");
            return;
        };
        for swing in [false, true] {
            let dobj = faith_as_dobj(&pack.skeleton, swing);
            let rt = Retarget::new(&pack.skeleton, &dobj).expect("core bones");
            assert!((rt.scale - 1.0).abs() < 1e-3);
            for clip in ["WallRunLeft", "VaultOverFast", "FallingLandRoll"] {
                let Some(c) = pack.clip(clip) else { continue };
                let mut pose = SrcPose::default();
                pack.clips[c].sample(0.5, &mut pose);
                let mut locals: Vec<anim_iw4::Local> = dobj
                    .bones
                    .iter()
                    .map(|b| anim_iw4::Local::uncontrolled(b.bind_rotation.to_array()))
                    .collect();
                rt.apply(&pack.skeleton, &pose, &dobj, &mut locals, |_| 1.0);
                let got = posed(&dobj, &locals);
                let (mut sq, mut sp) = (Vec::new(), Vec::new());
                pack.skeleton.world(&pose, &mut sq, &mut sp);
                for (src, dst) in [
                    ("LeftForeArm", "j_elbow_le"),
                    ("LeftHand", "j_wrist_le"),
                    ("RightFoot", "j_ankle_ri"),
                    ("Neck", "j_neck"), // j_head sits past the unmapped Neck1 here
                    ("LeftToeBase", "j_ball_le"),
                    ("Hips", "j_mainroot"),
                ] {
                    let a = sp[pack.skeleton.find(src).unwrap()];
                    let b = got[dobj.find(dst).unwrap()];
                    assert!(
                        (a - b).length() < 0.5,
                        "{clip} swing={swing} {dst}: {a} vs {b}"
                    );
                }
            }
        }
    }
}
