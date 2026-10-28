//! `fragment.merge`, and how meshes and polygon meshes become entities.

use std::sync::Arc;

use glam::Vec3;
use re_reflect::Value;
use re_types::mesh::MeshData;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Domain, NodeCtx, NodeDef, NodeError, NodeRegistry, NodeSpec, Params, natural_cmp};
use crate::value::{
    DerivedAsset, FragItem, Fragment, MeshValue, Output, PolyMesh, PortType, derived_name,
};

/// One entity drawing `mesh`: a default `Transform`, a `MeshRef` to the
/// derived mesh and, if the mesh has one, a `MaterialRef`. Returns the item
/// and the derived asset it references.
pub fn mesh_item(
    key: String,
    name: String,
    mesh: &MeshValue,
) -> (FragItem, (String, DerivedAsset)) {
    let asset = derived_name(&mesh.asset);
    let mut components = vec![
        ("Transform".to_string(), Vec::new()),
        (
            "MeshRef".to_string(),
            vec![("asset".to_string(), Value::Str(asset.clone()))],
        ),
    ];
    if !mesh.material.is_empty() {
        components.push((
            "MaterialRef".into(),
            vec![("asset".into(), Value::Str(mesh.material.clone()))],
        ));
    }
    (
        FragItem {
            key,
            name,
            parent_key: None,
            components,
            prefab: None,
        },
        (asset, DerivedAsset::Mesh(Arc::new(mesh.mesh.clone()))),
    )
}

/// [`mesh_item`] for a mesh output in a streamed window, if any: a mesh
/// there is local to the window's corner (`min x, 0, min z`), so its entity
/// sits at the corner, and an empty mesh (a cell no road crosses) makes no
/// entity.
fn window_mesh_item(
    key: String,
    name: String,
    mesh: &MeshValue,
    domain: Option<&Domain>,
) -> Option<(FragItem, (String, DerivedAsset))> {
    let Some(d) = domain else {
        return Some(mesh_item(key, name, mesh));
    };
    if mesh.mesh.indices.is_empty() {
        return None;
    }
    let (mut item, asset) = mesh_item(key, name, mesh);
    let [x, z] = d.min();
    item.components[0].1 = vec![("translation".into(), Value::Vec3(Vec3::new(x, 0.0, z)))];
    Some((item, asset))
}

/// The name of a polygon mesh part's derived mesh: a hash of its content,
/// so equal parts share one derived asset (and one GPU mesh).
fn part_asset(mesh: &MeshData) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(mesh.vertices.len() * 48 + mesh.indices.len() * 4 + 40);
    bytes.extend_from_slice(b"re_proc polymesh part");
    bytes.extend_from_slice(&(mesh.vertices.len() as u64).to_le_bytes());
    for v in &mesh.vertices {
        for x in v.pos.iter().chain(&v.normal).chain(&v.uv).chain(&v.color) {
            bytes.extend_from_slice(&x.to_bits().to_le_bytes());
        }
    }
    for i in &mesh.indices {
        bytes.extend_from_slice(&i.to_le_bytes());
    }
    *blake3::hash(&bytes).as_bytes()
}

/// The entities drawing a polygon mesh (ADR-0062 §6): one per material part
/// that has faces, keyed `key` for the first part and `<key>.<n>` for part
/// `n` after it (in the material table's order), each with a `Transform`, a
/// `MeshRef` to the part's derived mesh and, for a part with a material, a
/// `MaterialRef`. In a streamed window they sit at the window's corner.
/// `Err` (`RE4505`) when a face cannot be triangulated.
pub fn polymesh_fragment(
    key: &str,
    name: &str,
    mesh: &PolyMesh,
    domain: Option<&Domain>,
) -> Result<Fragment, NodeError> {
    let mut out = Fragment::default();
    for (i, part) in mesh.to_meshes()?.into_iter().enumerate() {
        if part.indices.is_empty() {
            continue;
        }
        let asset = derived_name(&part_asset(&part));
        let transform = match domain {
            Some(d) => {
                let [x, z] = d.min();
                vec![("translation".into(), Value::Vec3(Vec3::new(x, 0.0, z)))]
            }
            None => Vec::new(),
        };
        let mut components = vec![
            ("Transform".to_string(), transform),
            (
                "MeshRef".to_string(),
                vec![("asset".to_string(), Value::Str(asset.clone()))],
            ),
        ];
        let material = mesh.part_material(i);
        if !material.is_empty() {
            components.push((
                "MaterialRef".into(),
                vec![("asset".into(), Value::Str(material.to_string()))],
            ));
        }
        let (key, name) = if i == 0 {
            (key.to_string(), name.to_string())
        } else {
            (format!("{key}.{i}"), format!("{name}.{i}"))
        };
        out.items.push(FragItem {
            key,
            name,
            parent_key: None,
            components,
            prefab: None,
        });
        if !out.assets.iter().any(|(n, _)| *n == asset) {
            out.assets.push((asset, DerivedAsset::Mesh(Arc::new(part))));
        }
    }
    Ok(out)
}

/// The fragment a node's output adds to the graph's output when nothing
/// consumes it: a fragment as it is, a mesh as one entity `<node>/mesh`, a
/// polygon mesh as one entity per material part ([`polymesh_fragment`],
/// `<node>/mesh`, `<node>/mesh.1`, …); fields, points, shapes and paths add
/// nothing. `Err` when a polygon mesh cannot be triangulated.
pub fn sink_fragment(node_id: &str, output: &Output) -> Result<Option<Arc<Fragment>>, NodeError> {
    sink_fragment_in(node_id, output, None)
}

/// [`sink_fragment`] in a streamed window `domain` (ADR-0042): a mesh's
/// entities sit at the window's corner, and an empty mesh adds nothing.
pub fn sink_fragment_in(
    node_id: &str,
    output: &Output,
    domain: Option<&Domain>,
) -> Result<Option<Arc<Fragment>>, NodeError> {
    Ok(match output {
        Output::Fragment(f) => Some(f.clone()),
        Output::Mesh(m) => {
            window_mesh_item(format!("{node_id}/mesh"), node_id.to_string(), m, domain).map(
                |(item, asset)| {
                    Arc::new(Fragment {
                        items: vec![item],
                        assets: vec![asset],
                    })
                },
            )
        }
        Output::PolyMesh(m) => {
            let f = polymesh_fragment(&format!("{node_id}/mesh"), node_id, m, domain)?;
            (!f.items.is_empty()).then(|| Arc::new(f))
        }
        Output::Field(_) | Output::Points(_) | Output::Shapes(_) | Output::Path(_) => None,
    })
}

/// Parameters of `fragment.merge`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct FragmentMergeParams {
    /// When set, a group entity `<node>/group` with this name is created and
    /// every entity of the inputs without a parent goes under it.
    pub group: String,
}

impl Params for FragmentMergeParams {}

fn merge(ctx: &NodeCtx, p: &FragmentMergeParams) -> Result<Output, NodeError> {
    let mut inputs: Vec<&(String, Output)> = ctx.inputs.iter().collect();
    inputs.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let mut out = Fragment::default();
    let group = (!p.group.is_empty()).then(|| format!("{}/group", ctx.node_id));
    if let Some(key) = &group {
        out.items.push(FragItem {
            key: key.clone(),
            name: p.group.clone(),
            parent_key: None,
            components: vec![("Transform".into(), Vec::new())],
            prefab: None,
        });
    }
    for (port, output) in inputs {
        let part = match output {
            Output::Mesh(m) => {
                let key = format!("{}/{port}", ctx.node_id);
                let Some((item, asset)) =
                    window_mesh_item(key, port.clone(), m, ctx.domain.as_ref())
                else {
                    continue;
                };
                Arc::new(Fragment {
                    items: vec![item],
                    assets: vec![asset],
                })
            }
            Output::PolyMesh(m) => Arc::new(polymesh_fragment(
                &format!("{}/{port}", ctx.node_id),
                port,
                m,
                ctx.domain.as_ref(),
            )?),
            Output::Fragment(f) => f.clone(),
            _ => continue,
        };
        for item in &part.items {
            let mut item = item.clone();
            if item.parent_key.is_none() {
                item.parent_key = group.clone();
            }
            out.items.push(item);
        }
        for asset in &part.assets {
            if !out.assets.iter().any(|(name, _)| name == &asset.0) {
                out.assets.push(asset.clone());
            }
        }
    }
    Ok(Output::Fragment(Arc::new(out)))
}

pub(super) fn register(r: &mut NodeRegistry) {
    r.register(NodeDef::new(
        NodeSpec {
            kind: "fragment.merge",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Cell,
            doc: "Gathers fragments, meshes and polygon meshes (inputs on any port names, in natural name order) into one fragment, optionally under a group entity; a polygon mesh makes one entity per material part, `<node>/<port>` then `<node>/<port>.<n>`. Streamed: per cell; a mesh's entity sits at the cell's corner, and an empty mesh makes none.",
            inputs: &[],
            variadic: Some(&[PortType::Fragment, PortType::Mesh, PortType::PolyMesh]),
            output: PortType::Fragment,
            uses_seed: false,
            file_params: &[],
        },
        merge,
    ));
}
