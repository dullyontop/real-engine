//! Shader hot reload (ADR-0038): the pipelines of every shader file that
//! changed are rebuilt, and a change that does not build leaves the last good
//! pipelines in use.
//!
//! Each root file (one the renderer makes a shader module from) belongs to a
//! family of pipelines that are rebuilt together. A reload preprocesses every
//! root from the current sources, skips the ones that are what their pipelines
//! were built from, checks the rest with naga (`RE6031`) and rebuilds each family
//! with its changed roots under a validation error scope. A family the device
//! refuses (`RE6032`) is put back as it was. A rebuilt family has a pipeline for
//! each entry point it uses, for a linear and a gamma target as well as for the
//! formats drawn into so far, so the device has checked a reloaded shader before
//! a frame first needs it.

use std::collections::HashMap;

use re_core::{Diagnostic, Location};

use super::{
    Geometry, Renderer, ScenePipelines, mask_pipelines, probe_pipeline, shadow_pipeline,
    with_checks,
};
use crate::post::PostShader;
use crate::ui::UiBlit;
use crate::{codes, shader};

/// What [`Renderer::reload_shaders`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShaderReload {
    /// The root shader files whose pipelines were rebuilt from a changed source.
    pub rebuilt: Vec<&'static str>,
    /// Why the other changed files were not: `RE6030` (an include), `RE6031`
    /// (it does not compile) or `RE6032` (the GPU refused the pipelines). Their
    /// last good pipelines stay in use.
    pub problems: Vec<Diagnostic>,
}

/// Pipelines that are rebuilt together, and the root files they are made from.
#[derive(Clone, Copy, Debug)]
enum Family {
    /// [`ScenePipelines`] for each sample count, and the on-top quads.
    Scene,
    Highlight,
    Shadow,
    ShadowSkinned,
    ShadowSet,
    Mask,
    MaskSkinned,
    MaskSet,
    Post,
    Ui,
    Probe,
}

impl Family {
    const ALL: [Family; 11] = [
        Family::Scene,
        Family::Highlight,
        Family::Shadow,
        Family::ShadowSkinned,
        Family::ShadowSet,
        Family::Mask,
        Family::MaskSkinned,
        Family::MaskSet,
        Family::Post,
        Family::Ui,
        Family::Probe,
    ];

    fn roots(self) -> &'static [&'static str] {
        match self {
            Family::Scene => &[
                "forward.wgsl",
                "forward_skinned.wgsl",
                "forward_set.wgsl",
                "lines.wgsl",
                "effects.wgsl",
                "sky.wgsl",
                "quad.wgsl",
            ],
            Family::Highlight => &["highlight.wgsl"],
            Family::Shadow => &["shadow.wgsl"],
            Family::ShadowSkinned => &["shadow_skinned.wgsl"],
            Family::ShadowSet => &["shadow_set.wgsl"],
            Family::Mask => &["mask.wgsl"],
            Family::MaskSkinned => &["mask_skinned.wgsl"],
            Family::MaskSet => &["mask_set.wgsl"],
            Family::Post => &["post.wgsl"],
            Family::Ui => &["ui_blit.wgsl"],
            Family::Probe => &["probe.wgsl"],
        }
    }
}

/// True for the roots only GPU skinning builds.
fn needs_skinning(root: &str) -> bool {
    root.ends_with("_skinned.wgsl")
}

/// Every root file, preprocessed from the built-in sources.
///
/// # Panics
///
/// Panics if a built-in shader fails to preprocess, which a unit test rules out.
pub(super) fn built_in_roots() -> HashMap<&'static str, String> {
    Family::ALL
        .iter()
        .flat_map(|family| family.roots())
        .map(|&root| {
            let source = shader::preprocess(root, shader::SOURCES)
                .unwrap_or_else(|d| panic!("built-in shader `{root}` is broken: {d}"));
            (root, source)
        })
        .collect()
}

/// What [`Renderer::install`] replaced, for [`Renderer::restore`].
enum Saved {
    Scene {
        /// forward, lines, effects, sky, quad.
        modules: [wgpu::ShaderModule; 5],
        skinned: Option<wgpu::ShaderModule>,
        /// forward_set.
        set: wgpu::ShaderModule,
        pipelines: HashMap<u32, ScenePipelines>,
        top_quads: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    },
    Highlight(
        wgpu::ShaderModule,
        HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    ),
    Shadow(wgpu::RenderPipeline),
    ShadowSkinned(wgpu::RenderPipeline),
    ShadowSet(wgpu::RenderPipeline),
    Mask([wgpu::RenderPipeline; 2]),
    MaskSkinned([wgpu::RenderPipeline; 2]),
    MaskSet([wgpu::RenderPipeline; 2]),
    Post(PostShader),
    Ui(UiBlit),
    Probe(Option<wgpu::RenderPipeline>),
}

impl Renderer {
    /// Sets the shader file `name` to `source`: it replaces the built-in file of
    /// that name, or is a new file for shaders to include. `None` goes back to
    /// the built-in file, or drops an added one. It takes effect at the next
    /// [`Renderer::reload_shaders`].
    pub fn set_shader_source(&mut self, name: &str, source: Option<String>) {
        self.sources.set(name, source);
    }

    /// The shader files set with [`Renderer::set_shader_source`], over the
    /// built-in ones.
    pub fn shader_sources(&self) -> &shader::ShaderSources {
        &self.sources
    }

    /// Rebuilds the pipelines of every shader file whose source, with its
    /// includes, changed since they were built (ADR-0038). A file that does not
    /// build keeps its last good pipelines, and the report says why.
    pub fn reload_shaders(&mut self) -> ShaderReload {
        let mut report = ShaderReload::default();
        for family in Family::ALL {
            let mut changed: Vec<(&'static str, String)> = Vec::new();
            for &root in family.roots() {
                if needs_skinning(root) && self.skinning.is_none() {
                    continue;
                }
                let checked = self.sources.preprocess(root).and_then(|pre| {
                    if self.built.get(root) == Some(&pre.source) {
                        return Ok(None);
                    }
                    shader::validate(root, &pre).map(|()| Some(pre.source))
                });
                match checked {
                    Ok(Some(source)) => changed.push((root, source)),
                    Ok(None) => {}
                    Err(d) => {
                        if !report.problems.contains(&d) {
                            report.problems.push(d);
                        }
                    }
                }
            }
            if changed.is_empty() {
                continue;
            }
            let device = self.device.clone();
            let (saved, refused) = scoped(&device, || {
                let modules = changed
                    .iter()
                    .map(|(root, source)| {
                        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                            label: Some(root),
                            source: wgpu::ShaderSource::Wgsl(source.as_str().into()),
                        });
                        (*root, module)
                    })
                    .collect();
                self.install(family, modules)
            });
            match refused {
                None => {
                    for (root, source) in changed {
                        report.rebuilt.push(root);
                        self.built.insert(root, source);
                    }
                }
                Some(error) => {
                    self.restore(saved);
                    let files: Vec<String> =
                        changed.iter().map(|(r, _)| format!("`{r}`")).collect();
                    let mut d = Diagnostic::error(
                        codes::SHADER_REJECTED,
                        format!(
                            "the GPU refused the pipelines built from {}: {}",
                            files.join(", "),
                            device_message(&error)
                        ),
                    )
                    .with_hint(
                        "keep the entry points, bindings and outputs of the engine's file \
                         when replacing it; the last good pipelines stay in use",
                    );
                    if let [(root, _)] = changed.as_slice() {
                        d = d.at(Location::file(*root));
                    }
                    report.problems.push(d);
                }
            }
        }
        report
    }

    /// Swaps the `modules` of `family`'s changed roots in and rebuilds its
    /// pipelines with them.
    fn install(
        &mut self,
        family: Family,
        modules: Vec<(&'static str, wgpu::ShaderModule)>,
    ) -> Saved {
        let only = || modules[0].1.clone();
        match family {
            Family::Scene => {
                let mut samples: Vec<u32> = self.pipelines.keys().copied().collect();
                if samples.is_empty() {
                    samples.push(self.sample_count());
                }
                let formats = with_checks(self.top_quad_pipelines.keys());
                let saved = Saved::Scene {
                    modules: [
                        self.forward_shader.clone(),
                        self.lines_shader.clone(),
                        self.effects_shader.clone(),
                        self.sky_shader.clone(),
                        self.quad_shader.clone(),
                    ],
                    skinned: self.skinning.as_ref().map(|s| s.forward_shader.clone()),
                    set: self.sets.forward_shader.clone(),
                    pipelines: std::mem::take(&mut self.pipelines),
                    top_quads: std::mem::take(&mut self.top_quad_pipelines),
                };
                for (root, module) in modules {
                    match root {
                        "forward.wgsl" => self.forward_shader = module,
                        "lines.wgsl" => self.lines_shader = module,
                        "effects.wgsl" => self.effects_shader = module,
                        "sky.wgsl" => self.sky_shader = module,
                        "quad.wgsl" => self.quad_shader = module,
                        "forward_set.wgsl" => self.sets.forward_shader = module,
                        _ => {
                            if let Some(s) = &mut self.skinning {
                                s.forward_shader = module;
                            }
                        }
                    }
                }
                for samples in samples {
                    let pipelines = self.create_pipelines(samples);
                    self.pipelines.insert(samples, pipelines);
                }
                for format in formats {
                    let pipeline = self.quad_pipeline(format, 1, false);
                    self.top_quad_pipelines.insert(format, pipeline);
                }
                saved
            }
            Family::Highlight => {
                let formats = with_checks(self.highlight_pipelines.keys());
                let saved = Saved::Highlight(
                    std::mem::replace(&mut self.highlight_shader, only()),
                    std::mem::take(&mut self.highlight_pipelines),
                );
                for format in formats {
                    let pipeline = self.create_highlight_pipeline(format);
                    self.highlight_pipelines.insert(format, pipeline);
                }
                saved
            }
            Family::Shadow => {
                let pipeline =
                    shadow_pipeline(&self.device, &self.layouts.shadow, &only(), Geometry::Items);
                Saved::Shadow(std::mem::replace(&mut self.shadow_pipeline, pipeline))
            }
            Family::ShadowSkinned => {
                let s = self.skinning.as_mut().expect("skinned roots need skinning");
                let pipeline =
                    shadow_pipeline(&self.device, &s.shadow_layout, &only(), Geometry::Skinned);
                Saved::ShadowSkinned(std::mem::replace(&mut s.shadow_pipeline, pipeline))
            }
            Family::ShadowSet => {
                let s = &mut self.sets;
                let pipeline =
                    shadow_pipeline(&self.device, &s.shadow_layout, &only(), Geometry::Sets);
                Saved::ShadowSet(std::mem::replace(&mut s.shadow_pipeline, pipeline))
            }
            Family::Mask => {
                let pipelines =
                    mask_pipelines(&self.device, &self.layouts.mask, &only(), Geometry::Items);
                Saved::Mask(std::mem::replace(&mut self.mask_pipelines, pipelines))
            }
            Family::MaskSkinned => {
                let s = self.skinning.as_mut().expect("skinned roots need skinning");
                let pipelines =
                    mask_pipelines(&self.device, &s.mask_layout, &only(), Geometry::Skinned);
                Saved::MaskSkinned(std::mem::replace(&mut s.mask_pipelines, pipelines))
            }
            Family::MaskSet => {
                let s = &mut self.sets;
                let pipelines =
                    mask_pipelines(&self.device, &s.mask_layout, &only(), Geometry::Sets);
                Saved::MaskSet(std::mem::replace(&mut s.mask_pipelines, pipelines))
            }
            Family::Post => Saved::Post(self.post.replace_shader(&self.device, only())),
            Family::Ui => Saved::Ui(self.ui.replace_blit(&self.device, only())),
            Family::Probe => {
                let pipeline = probe_pipeline(&self.device, &self.layouts.frame_only, &only());
                Saved::Probe(self.probe.replace(pipeline))
            }
        }
    }

    /// Puts back what [`Renderer::install`] replaced.
    fn restore(&mut self, saved: Saved) {
        match saved {
            Saved::Scene {
                modules: [forward, lines, effects, sky, quad],
                skinned,
                set,
                pipelines,
                top_quads,
            } => {
                self.forward_shader = forward;
                self.lines_shader = lines;
                self.effects_shader = effects;
                self.sky_shader = sky;
                self.quad_shader = quad;
                if let (Some(s), Some(module)) = (&mut self.skinning, skinned) {
                    s.forward_shader = module;
                }
                self.sets.forward_shader = set;
                self.pipelines = pipelines;
                self.top_quad_pipelines = top_quads;
            }
            Saved::Highlight(shader, pipelines) => {
                self.highlight_shader = shader;
                self.highlight_pipelines = pipelines;
            }
            Saved::Shadow(pipeline) => self.shadow_pipeline = pipeline,
            Saved::ShadowSkinned(pipeline) => {
                if let Some(s) = &mut self.skinning {
                    s.shadow_pipeline = pipeline;
                }
            }
            Saved::ShadowSet(pipeline) => self.sets.shadow_pipeline = pipeline,
            Saved::Mask(pipelines) => self.mask_pipelines = pipelines,
            Saved::MaskSkinned(pipelines) => {
                if let Some(s) = &mut self.skinning {
                    s.mask_pipelines = pipelines;
                }
            }
            Saved::MaskSet(pipelines) => self.sets.mask_pipelines = pipelines,
            Saved::Post(old) => self.post.restore_shader(old),
            Saved::Ui(old) => self.ui.restore_blit(old),
            Saved::Probe(pipeline) => self.probe = pipeline,
        }
    }
}

/// Runs `build` and returns the validation error the device reports against
/// it. Native devices report at once. On the web the report would come later,
/// so there errors reach the device's uncaptured-error handler (`RE6001`) and
/// nothing is put back.
fn scoped<T>(device: &wgpu::Device, build: impl FnOnce() -> T) -> (T, Option<wgpu::Error>) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let built = build();
        (built, pollster::block_on(scope.pop()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = device;
        (build(), None)
    }
}

/// The device's error as a chain of causes, without wgpu's banner.
fn device_message(error: &wgpu::Error) -> String {
    match error {
        wgpu::Error::Validation { source, .. } => shader::error_chain(source.as_ref()),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_families_build_every_root_once() {
        let mut roots: Vec<&str> = Family::ALL
            .iter()
            .flat_map(|f| f.roots())
            .copied()
            .collect();
        roots.sort_unstable();
        let mut listed = shader::ROOTS.to_vec();
        listed.sort_unstable();
        assert_eq!(roots, listed);
    }
}
