//! `fragment.merge`, and how meshes become entities.

use std::sync::Arc;

use re_reflect::Value;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{NodeCtx, NodeDef, NodeRegistry, NodeSpec, Params, natural_cmp};
use crate::value::{DerivedAsset, FragItem, Fragment, MeshValue, Output, PortType, derived_name};

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

/// The fragment a node's output adds to the graph's output when nothing
/// consumes it: a fragment as it is, a mesh as one entity `<node>/mesh`;
/// fields and points add nothing.
pub fn sink_fragment(node_id: &str, output: &Output) -> Option<Arc<Fragment>> {
    match output {
        Output::Fragment(f) => Some(f.clone()),
        Output::Mesh(m) => {
            let (item, asset) = mesh_item(format!("{node_id}/mesh"), node_id.to_string(), m);
            Some(Arc::new(Fragment {
                items: vec![item],
                assets: vec![asset],
            }))
        }
        Output::Field(_) | Output::Points(_) => None,
    }
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

fn merge(ctx: &NodeCtx, p: &FragmentMergeParams) -> Result<Output, String> {
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
                let (item, asset) = mesh_item(format!("{}/{port}", ctx.node_id), port.clone(), m);
                Arc::new(Fragment {
                    items: vec![item],
                    assets: vec![asset],
                })
            }
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
            streaming: super::Streaming::Whole,
            doc: "Gathers fragments and meshes (inputs on any port names, in natural name order) into one fragment, optionally under a group entity.",
            inputs: &[],
            variadic: Some(&[PortType::Fragment, PortType::Mesh]),
            output: PortType::Fragment,
            uses_seed: false,
            file_params: &[],
        },
        merge,
    ));
}
