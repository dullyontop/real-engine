//! glTF skins (ADR-0039): joints, inverse bind matrices and per-vertex weights.
//!
//! [`node_names`] gives every node the name clip targets use, renaming joints
//! whose names collide. [`read_weights`] reads a primitive's `JOINTS_n` and
//! `WEIGHTS_n` into [`SkinWeights`] (four influences, unorm16, summing to one).
//! [`import_skins`] builds a [`SkinData`] per skin, with its joints ordered so
//! that every parent comes first, remaps the weights of the meshes it skins to
//! that order, and records which skin each node uses.

use std::collections::{BTreeMap, BTreeSet};

use glam::{Mat4, Vec3};
use re_core::{Diagnostic, Location};
use re_types::anim::{JointDesc, SkinData, Trs};
use re_types::mesh::{Aabb, SkinWeights};

use super::{ImportedMesh, ImportedNode};
use crate::codes;

/// The number of influences a vertex keeps.
const INFLUENCES: usize = 4;

/// The name of every node: its glTF name or `node_<index>`, with a joint whose
/// name another node also has renamed `<name>#<index>` (one warning per name).
pub(crate) fn node_names(
    document: &gltf::Document,
    file: &str,
    warnings: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let mut names: Vec<String> = document
        .nodes()
        .map(|n| {
            n.name()
                .map_or_else(|| format!("node_{}", n.index()), str::to_owned)
        })
        .collect();
    let joints: BTreeSet<usize> = document
        .skins()
        .flat_map(|s| s.joints().map(|j| j.index()).collect::<Vec<_>>())
        .collect();
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for n in &names {
        *count.entry(n.as_str()).or_default() += 1;
    }
    let clashing: BTreeSet<String> = joints
        .iter()
        .filter(|&&j| count[names[j].as_str()] > 1)
        .map(|&j| names[j].clone())
        .collect();
    for name in &clashing {
        warnings.push(
            Diagnostic::warning(
                codes::SKIN_DUPLICATE_JOINT_NAME,
                format!(
                    "several nodes are called `{name}`; its joints are renamed `{name}#<node index>`"
                ),
            )
            .at(Location::file(file))
            .with_hint("give every joint a unique name in the modelling tool"),
        );
    }
    for &j in &joints {
        if clashing.contains(&names[j]) {
            names[j] = format!("{}#{j}", names[j]);
        }
    }
    names
}

/// The skin weights of a primitive with `n` vertices, or `None` when it has no
/// `JOINTS_0` and `WEIGHTS_0`. A vertex keeps its four largest influences;
/// `too_many` counts the vertices that had more.
///
/// # Errors
///
/// [`codes::SKIN_INVALID`] when the attributes have the wrong type or length, or
/// only one of a `JOINTS_n`/`WEIGHTS_n` pair exists.
pub(crate) fn read_weights<'a, 's, F>(
    prim: &gltf::Primitive<'a>,
    reader: &gltf::mesh::Reader<'a, 's, F>,
    n: usize,
    mesh: &str,
    file: &str,
    too_many: &mut usize,
) -> Result<Option<SkinWeights>, Diagnostic>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    use gltf::Semantic;
    use gltf::accessor::{DataType, Dimensions};
    let bad = |message: String| {
        Diagnostic::error(codes::SKIN_INVALID, format!("mesh `{mesh}`: {message}"))
            .at(Location::file(file))
    };
    let mut influences: Vec<Vec<(u16, f32)>> = vec![Vec::new(); n];
    let mut sets = 0;
    for set in 0.. {
        let (joints, weights) = (
            prim.get(&Semantic::Joints(set)),
            prim.get(&Semantic::Weights(set)),
        );
        let (joints, weights) = match (joints, weights) {
            (None, None) => break,
            (Some(j), Some(w)) => (j, w),
            _ => {
                return Err(bad(format!(
                    "JOINTS_{set} and WEIGHTS_{set} must come together"
                )));
            }
        };
        if joints.dimensions() != Dimensions::Vec4
            || !matches!(joints.data_type(), DataType::U8 | DataType::U16)
        {
            return Err(bad(format!(
                "JOINTS_{set} must be a VEC4 of unsigned bytes or shorts"
            )));
        }
        let float_ok = weights.data_type() == DataType::F32;
        let unorm_ok =
            matches!(weights.data_type(), DataType::U8 | DataType::U16) && weights.normalized();
        if weights.dimensions() != Dimensions::Vec4 || !(float_ok || unorm_ok) {
            return Err(bad(format!(
                "WEIGHTS_{set} must be a VEC4 of floats or normalized unsigned bytes or shorts"
            )));
        }
        if joints.count() != n || weights.count() != n {
            return Err(bad(format!(
                "JOINTS_{set} has {} and WEIGHTS_{set} {} entries for {n} vertices",
                joints.count(),
                weights.count()
            )));
        }
        let (Some(js), Some(ws)) = (reader.read_joints(set), reader.read_weights(set)) else {
            return Err(bad(format!("JOINTS_{set} or WEIGHTS_{set} has no data")));
        };
        for ((v, j), w) in influences.iter_mut().zip(js.into_u16()).zip(ws.into_f32()) {
            for k in 0..4 {
                let w = w[k];
                if w.is_finite() && w > 0.0 {
                    match v.iter_mut().find(|(joint, _)| *joint == j[k]) {
                        Some((_, sum)) => *sum += w,
                        None => v.push((j[k], w)),
                    }
                }
            }
        }
        sets += 1;
    }
    if sets == 0 {
        return Ok(None);
    }
    let mut out = SkinWeights {
        joints: Vec::with_capacity(n),
        weights: Vec::with_capacity(n),
    };
    for mut v in influences {
        // Largest first; equal weights keep the lower joint first.
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        if v.len() > INFLUENCES {
            *too_many += 1;
            v.truncate(INFLUENCES);
        }
        let (j, w) = quantize(&v);
        out.joints.push(j);
        out.weights.push(w);
    }
    Ok(Some(out))
}

/// Up to four `(joint, weight)` pairs, largest first, as unorm16 weights that sum
/// to exactly 65,535. The rounding remainder goes to the largest weight; no
/// weight at all binds the vertex fully to joint 0.
fn quantize(influences: &[(u16, f32)]) -> ([u16; 4], [u16; 4]) {
    let mut joints = [0u16; 4];
    let mut weights = [0u16; 4];
    let sum: f32 = influences.iter().map(|&(_, w)| w).sum();
    if influences.is_empty() || sum <= 0.0 || !sum.is_finite() {
        weights[0] = u16::MAX;
        return (joints, weights);
    }
    let mut total: i64 = 0;
    for (k, &(j, w)) in influences.iter().enumerate() {
        joints[k] = j;
        let q = (w / sum * 65535.0).round().clamp(0.0, 65535.0) as u16;
        weights[k] = q;
        total += i64::from(q);
    }
    let fixed = i64::from(weights[0]) + (65535 - total);
    weights[0] = fixed.clamp(0, 65535) as u16;
    (joints, weights)
}

/// Every skin of the document as a [`SkinData`]. Sets [`ImportedNode::skin`] on
/// the nodes that use a skin, remaps their meshes' weights to the skin's joint
/// order (copying a mesh that two skins share) and fills the joint bounds.
///
/// # Errors
///
/// [`codes::SKIN_INVALID`] for a skin without joints, with a joint listed twice,
/// with too few inverse bind matrices, with more than 65,535 joints, or with a
/// mesh whose weights name a joint the skin does not have.
pub(crate) fn import_skins<'a, 's, F>(
    document: &'a gltf::Document,
    buffer: F,
    names: &[String],
    meshes: &mut Vec<ImportedMesh>,
    nodes: &mut [ImportedNode],
    file: &str,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Vec<SkinData>, Diagnostic>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    let at = || Location::file(file);
    let local: Vec<Mat4> = document
        .nodes()
        .map(|n| Mat4::from_cols_array_2d(&n.transform().matrix()))
        .collect();
    let mut skins = Vec::new();
    // Per skin: old (glTF) joint index -> new index.
    let mut remaps: Vec<Vec<u16>> = Vec::new();
    for skin in document.skins() {
        let s = skin.index();
        let name = skin
            .name()
            .map_or_else(|| format!("skin_{s}"), str::to_owned);
        let bad = |message: String| {
            Diagnostic::error(codes::SKIN_INVALID, format!("skin `{name}`: {message}")).at(at())
        };
        let gltf_joints: Vec<usize> = skin.joints().map(|j| j.index()).collect();
        if gltf_joints.is_empty() {
            return Err(bad("it has no joints".into()));
        }
        if gltf_joints.len() > u16::MAX as usize {
            return Err(bad(format!(
                "it has {} joints; at most 65,535 are supported",
                gltf_joints.len()
            )));
        }
        let mut position: BTreeMap<usize, usize> = BTreeMap::new();
        for (k, &node) in gltf_joints.iter().enumerate() {
            if position.insert(node, k).is_some() {
                return Err(bad(format!("joint `{}` is listed twice", names[node])));
            }
        }
        // The nearest joint ancestor of each joint, and the transform of the
        // non-joint nodes on the way to it (or to the scene root).
        let mut parent: Vec<Option<usize>> = Vec::with_capacity(gltf_joints.len());
        let mut offset: Vec<Mat4> = Vec::with_capacity(gltf_joints.len());
        // gltf does not reject cyclic node graphs, so each walk is bounded.
        let cyclic = || bad("its joints' node hierarchy has a cycle".into());
        for (k, &node) in gltf_joints.iter().enumerate() {
            let mut m = Mat4::IDENTITY;
            let mut up = nodes[node].parent;
            let mut found = None;
            let mut steps = 0;
            while let Some(p) = up {
                steps += 1;
                if steps > nodes.len() {
                    return Err(cyclic());
                }
                if let Some(&j) = position.get(&p) {
                    found = Some(j);
                    break;
                }
                m = local[p] * m;
                up = nodes[p].parent;
            }
            if found == Some(k) {
                return Err(cyclic());
            }
            parent.push(found);
            offset.push(m);
        }
        // Keep the file's order when it already has parents first; otherwise
        // order by depth, which does (stable, so siblings keep their order).
        let ordered = parent
            .iter()
            .enumerate()
            .all(|(k, p)| p.is_none_or(|p| p < k));
        let order: Vec<usize> = if ordered {
            (0..gltf_joints.len()).collect()
        } else {
            // Iterative, as joint chains can be long; a chain longer than the
            // joint count is a cycle.
            let mut depth = vec![usize::MAX; gltf_joints.len()];
            for k in 0..gltf_joints.len() {
                let mut chain = vec![k];
                while let Some(p) = parent[*chain.last().expect("non-empty")] {
                    if depth[p] != usize::MAX {
                        break;
                    }
                    if chain.len() > gltf_joints.len() {
                        return Err(cyclic());
                    }
                    chain.push(p);
                }
                for &j in chain.iter().rev() {
                    depth[j] = parent[j].map_or(0, |p| depth[p] + 1);
                }
            }
            let mut order: Vec<usize> = (0..gltf_joints.len()).collect();
            order.sort_by_key(|&k| depth[k]);
            order
        };
        let mut remap = vec![0u16; gltf_joints.len()];
        for (new, &old) in order.iter().enumerate() {
            remap[old] = new as u16;
        }

        let inverse_bind: Vec<Mat4> = match skin.inverse_bind_matrices() {
            None => vec![Mat4::IDENTITY; gltf_joints.len()],
            Some(acc) => {
                use gltf::accessor::{DataType, Dimensions};
                if acc.data_type() != DataType::F32 || acc.dimensions() != Dimensions::Mat4 {
                    return Err(bad("inverse bind matrices must be MAT4 floats".into()));
                }
                if acc.count() < gltf_joints.len() {
                    return Err(bad(format!(
                        "it has {} joints but {} inverse bind matrices",
                        gltf_joints.len(),
                        acc.count()
                    )));
                }
                let read: Vec<Mat4> =
                    gltf::accessor::Iter::<[[f32; 4]; 4]>::new(acc, buffer.clone())
                        .ok_or_else(|| bad("its inverse bind matrices have no data".into()))?
                        .map(|m| Mat4::from_cols_array_2d(&m))
                        .collect();
                order.iter().map(|&old| read[old]).collect()
            }
        };
        let joints = order
            .iter()
            .map(|&old| {
                let node = &nodes[gltf_joints[old]];
                let m = offset[old];
                JointDesc {
                    name: names[gltf_joints[old]].clone(),
                    parent: parent[old].map(|p| remap[p]),
                    rest: Trs {
                        translation: node.translation,
                        rotation: node.rotation,
                        scale: node.scale,
                    },
                    parent_offset: (m != Mat4::IDENTITY).then_some(m),
                }
            })
            .collect();
        skins.push(SkinData {
            name,
            joints,
            inverse_bind,
            joint_bounds: vec![None; gltf_joints.len()],
        });
        remaps.push(remap);
    }

    // Bind each skinned node's meshes to its skin.
    let mut owner: Vec<Option<usize>> = vec![None; meshes.len()];
    // Copies are made from the weights as imported, before any skin's remap.
    let pristine: Vec<Option<SkinWeights>> =
        meshes.iter().map(|m| m.skin_weights.clone()).collect();
    // The copy of mesh `m` made for skin `s`.
    let mut copies: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    let mut unweighted = BTreeSet::new();
    for node in document.nodes() {
        let Some(skin) = node.skin() else { continue };
        let s = skin.index();
        let i = node.index();
        nodes[i].skin = Some(s);
        for slot in 0..nodes[i].meshes.len() {
            let mut m = nodes[i].meshes[slot];
            if meshes[m].skin_weights.is_none() {
                unweighted.insert(meshes[m].name.clone());
                continue;
            }
            match owner[m] {
                Some(o) if o == s => continue,
                Some(_) => {
                    // Another skin already owns this mesh's weights: use or make
                    // this skin's copy.
                    if let Some(&c) = copies.get(&(m, s)) {
                        nodes[i].meshes[slot] = c;
                        continue;
                    }
                    let mut copy = meshes[m].clone();
                    copy.skin_weights = pristine[m].clone();
                    meshes.push(copy);
                    owner.push(None);
                    copies.insert((m, s), meshes.len() - 1);
                    m = meshes.len() - 1;
                    nodes[i].meshes[slot] = m;
                }
                None => {}
            }
            owner[m] = Some(s);
            let remap = &remaps[s];
            let mesh = &mut meshes[m];
            let weights = mesh.skin_weights.as_mut().expect("checked above");
            for (j, w) in weights.joints.iter_mut().zip(&weights.weights) {
                for k in 0..4 {
                    if w[k] == 0 {
                        j[k] = 0;
                        continue;
                    }
                    let Some(&new) = remap.get(j[k] as usize) else {
                        return Err(Diagnostic::error(
                            codes::SKIN_INVALID,
                            format!(
                                "mesh `{}` uses joint {} but skin `{}` has {} joints",
                                mesh.name,
                                j[k],
                                skins[s].name,
                                remap.len()
                            ),
                        )
                        .at(at()));
                    };
                    j[k] = new;
                }
            }
            let bounds = &mut skins[s].joint_bounds;
            for (v, (j, w)) in mesh
                .mesh
                .vertices
                .iter()
                .zip(weights.joints.iter().zip(&weights.weights))
            {
                let p = Vec3::from_array(v.pos);
                for k in 0..4 {
                    if w[k] > 0 {
                        let b = &mut bounds[j[k] as usize];
                        *b = Some(match b {
                            Some(b) => b.union(&Aabb::new(p, p)),
                            None => Aabb::new(p, p),
                        });
                    }
                }
            }
        }
    }
    for name in unweighted {
        warnings.push(
            Diagnostic::warning(
                codes::SKIN_UNWEIGHTED_MESH,
                format!("mesh `{name}` is on a skinned node but has no JOINTS_0 and WEIGHTS_0; it is drawn unskinned"),
            )
            .at(at()),
        );
    }
    debug_assert!(skins.iter().all(SkinData::is_well_formed));
    Ok(skins)
}

#[cfg(test)]
mod tests {
    use super::quantize;

    #[test]
    fn quantized_weights_sum_to_one() {
        let (j, w) = quantize(&[(3, 0.5), (1, 0.3), (2, 0.2)]);
        assert_eq!(j, [3, 1, 2, 0]);
        assert_eq!(w.iter().map(|&x| u32::from(x)).sum::<u32>(), 65535);
        let (_, w) = quantize(&[(0, 1.0), (1, 1.0), (2, 1.0)]);
        assert_eq!(w.iter().map(|&x| u32::from(x)).sum::<u32>(), 65535);
        assert_eq!((w[1], w[2]), (21845, 21845));
    }

    #[test]
    fn no_weight_binds_to_joint_zero() {
        assert_eq!(quantize(&[]), ([0; 4], [65535, 0, 0, 0]));
    }
}
