//! `re_assets`: Real Engine's asset pipeline.
//!
//! * [`AssetId`]: stable ids. Source files get a UUID in a `.meta.json` sidecar
//!   ([`AssetMeta`]); parts of files (the images in a glTF) get derived ids.
//! * [`import_gltf`]: glTF 2.0 meshes ([`MeshData`]), materials
//!   ([`MaterialDesc`]), images, the node tree, node animations ([`ClipData`])
//!   and skins ([`SkinData`]).
//! * [`load_material`]: `materials/*.material.json` files, with textures as
//!   [`TextureRef::Asset`] ids that `re_app` uploads and swaps for handles.
//! * [`ProcessedCache`]: engine-ready imports keyed by a hash of the importer
//!   version, settings and source bytes (postcard, written atomically).
//! * [`AssetServer`]: loads assets by project path behind generational
//!   [`Handle`]s and re-imports them when [`AssetWatcher`] reports changes.
//!
//! ```no_run
//! use re_assets::{AssetServer, AssetWatcher};
//!
//! let mut assets = AssetServer::new("my-game");
//! let tree = assets.load_gltf("assets/models/tree.glb")?;
//! let watcher = AssetWatcher::new(assets.root(), AssetWatcher::DEFAULT_DEBOUNCE)?;
//! // Each frame:
//! let report = assets.apply_changes(&watcher.poll());
//! for script in &report.scripts { /* reload it */ }
//! # Ok::<(), re_core::Diagnostic>(())
//! ```
//!
//! Loading is synchronous. With the `texture-encode` feature (tooling builds),
//! imported textures are stored as BC7 and BC5 with their mip chains, as
//! `[assets]` in `real.toml` asks ([`AssetsConfig`], ADR-0047). Errors are
//! [`re_core::Diagnostic`]s with codes RE7000–RE7199; each has an explain page
//! listed in [`DIAGNOSTICS`].
//!
//! [`MeshData`]: re_types::MeshData
//! [`MaterialDesc`]: re_types::MaterialDesc
//! [`ClipData`]: re_types::ClipData
//! [`SkinData`]: re_types::SkinData
//! [`TextureRef::Asset`]: re_types::TextureRef::Asset

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cache;
pub mod codes;
mod fsutil;
mod gltf;
mod id;
mod image_decode;
mod material;
mod meta;
mod server;
mod texture;
mod watch;

pub use self::gltf::{
    GLTF_IMPORTER_VERSION, ImportedGltf, ImportedMaterial, ImportedMesh, ImportedNode,
    ImportedTexture, image_id, import_gltf,
};
pub use cache::{
    CACHE_FORMAT_VERSION, CacheOutcome, ProcessedCache, cache_key, decode_entry, encode_entry,
};
pub use id::{AssetId, normalize_path};
pub use image_decode::decode_image;
pub use material::{
    AlphaModeName, MaterialFile, load_material, load_material_from, load_material_in,
};
pub use meta::AssetMeta;
pub use server::{
    AssetInfo, AssetServer, GLTF_IMPORTER, Handle, LoadedAsset, ProcessedAsset, ProcessedTexture,
    ReloadReport, TEXTURE_IMPORTER, TEXTURE_IMPORTER_VERSION, TEXTURE_REPEAT_AT_LOAD,
};
pub use texture::{AssetsConfig, TEXTURE_ENCODER, TextureCompression, TextureQuality};
pub use watch::{AssetChange, AssetWatcher};

use re_core::DiagInfo;

macro_rules! catalogue {
    ($($code:ident, $title:literal, $page:literal;)*) => {
        /// Every diagnostic code this crate uses, with its explain page.
        pub const DIAGNOSTICS: &[DiagInfo] = &[
            $(DiagInfo { code: codes::$code, title: $title, page: include_str!($page) },)*
        ];
    };
}

catalogue! {
    READ_FAILED, "Asset file cannot be read", "../explain/RE7001.md";
    GLTF_INVALID, "Invalid glTF file", "../explain/RE7002.md";
    IMAGE_INVALID, "Image cannot be decoded", "../explain/RE7003.md";
    MISSING_POSITIONS, "Mesh has no positions", "../explain/RE7004.md";
    GLTF_PART_SKIPPED, "Part of a glTF file was skipped", "../explain/RE7005.md";
    MATERIAL_INVALID, "Invalid material file", "../explain/RE7010.md";
    MATERIAL_TEXTURE_MISSING, "Material texture not found", "../explain/RE7011.md";
    MATERIAL_VALUE_OUT_OF_RANGE, "Material value out of range", "../explain/RE7012.md";
    META_INVALID, "Invalid asset sidecar", "../explain/RE7020.md";
    WRITE_FAILED, "Asset file cannot be written", "../explain/RE7021.md";
    WATCH_FAILED, "File watching cannot start", "../explain/RE7030.md";
    PATH_INVALID, "Invalid asset path", "../explain/RE7040.md";
    UNKNOWN_ASSET, "Unknown asset", "../explain/RE7041.md";
    UNSUPPORTED_ASSET, "Unsupported asset type", "../explain/RE7042.md";
    RELOAD_FAILED, "Hot reload failed", "../explain/RE7050.md";
    SKIN_INVALID, "Malformed glTF skin", "../explain/RE7060.md";
    SKIN_TOO_MANY_INFLUENCES, "More than four joint influences", "../explain/RE7061.md";
    SKIN_DUPLICATE_JOINT_NAME, "Joint names are not unique", "../explain/RE7062.md";
    SKIN_UNWEIGHTED_MESH, "Skinned mesh without weights", "../explain/RE7063.md";
}

#[cfg(test)]
mod tests {
    #[test]
    fn catalogue_is_valid() {
        let problems = re_core::diag::validate_catalog(super::DIAGNOSTICS, 7000..=7199);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
