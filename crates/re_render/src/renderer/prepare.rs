//! Preparing a frame: the items are resolved, culled, sorted and batched, the
//! sun's cascades fitted and the lights chosen, and the frame's buffers
//! written, for the shadow, scene and post passes to record.
//!
//! The lists live in a [`PreparedFrame`] that the renderer keeps between
//! frames, so a frame allocates nothing once they have grown, and the bind
//! groups a frame needs are kept by their inputs.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::num::NonZeroU64;
use std::ops::Range;

use glam::{Mat4, Vec2, Vec3, Vec4};
use re_core::Diagnostic;
use re_types::mesh::Aabb;
use re_types::render::{DrawFlags, MaterialHandle, MeshHandle, QuadDepth, RenderSnapshot};

use super::{
    CascadeUniform, FrameReport, MAX_PALETTE, QuadUniform, Renderer, SKINNED, Skinning, stale_item,
};
use crate::extras::{self, MAX_HIGHLIGHTS, SceneExtras};
use crate::frame::{self, FrameUniform, InstanceRaw, LineVertex, PointLightRaw, SpotLightRaw};
use crate::{codes, instances};

/// A resource slot: `(index, generation)`.
pub(crate) type Slot = (u32, u32);

/// One batch: consecutive instances of one mesh and, in the scene pass, one
/// material.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Batch {
    /// The forward pipeline (`double_sided | blend << 1 | SKINNED`); the shadow
    /// and mask batches use only `SKINNED`.
    pub(crate) variant: usize,
    pub(crate) material: Slot,
    pub(crate) mesh: Slot,
    /// The batch's first instance in the frame's instance buffer.
    pub(crate) first_instance: u32,
    pub(crate) instances: u32,
}

/// A draw item or a skinned draw item, as the frame resolves them.
#[derive(Clone, Copy)]
struct Draw {
    entity: u64,
    mesh: MeshHandle,
    material: MaterialHandle,
    world: Mat4,
    flags: DrawFlags,
    /// A skinned item's palette start, joint count and world bounds.
    skinned: Option<(u32, u32, Aabb)>,
}

/// A scene batch's sort key: the forward variant (with `SKINNED`), the
/// material's index and the mesh's index.
type Key = (usize, u32, u32);

/// The camera's view of a frame.
pub(super) struct View {
    pub(super) projection: Mat4,
    pub(super) view_proj: Mat4,
    pub(super) camera_pos: Vec3,
}

impl View {
    /// The view of `snapshot`'s camera into a `width` x `height` target. Fails
    /// (`RE6007`) when the camera cannot be projected.
    pub(super) fn new(
        snapshot: &RenderSnapshot,
        (width, height): (u32, u32),
    ) -> Result<View, Diagnostic> {
        let aspect = width as f32 / height as f32;
        let projection = frame::projection(&snapshot.camera, aspect)?;
        Ok(View {
            projection,
            view_proj: projection * snapshot.camera.view(),
            camera_pos: snapshot.camera.position(),
        })
    }
}

/// A frame prepared for its passes: what each pass draws, with the frame's
/// buffers written. It lives until the frame is submitted; the renderer keeps
/// its lists for the next frame.
#[derive(Default)]
pub(crate) struct PreparedFrame {
    /// The scene pass's batches: the opaque ones, then the blended ones back to
    /// front.
    pub(crate) batches: Vec<Batch>,
    /// How many of `batches` are opaque.
    pub(crate) opaque_batches: usize,
    /// The joint palette's bind group, when a batch is skinned.
    pub(crate) palette: Option<wgpu::BindGroup>,
    /// The MSAA sample count of the scene pass.
    pub(super) samples: u32,
    /// The target's size.
    pub(super) size: (u32, u32),
    /// Whether the sky is drawn (else the clear colour shows behind the scene).
    pub(super) sky: bool,
    /// The sun's place on screen, for sun rays.
    pub(super) sun_uv: Option<Vec2>,
    /// Depth-tested world quads in drawing order: the batch each is drawn
    /// before, and the quad.
    pub(super) tested_quads: Vec<(usize, usize)>,
    /// On-top world quads, in snapshot order.
    pub(super) top_quads: Vec<usize>,
    /// Each world quad's bind group, used at the quad's uniform offset.
    pub(super) quad_groups: Vec<wgpu::BindGroup>,
    /// The distance between two quads' uniforms.
    pub(super) quad_slot: u64,
    /// The highlighted items' batches, the occluded ones first.
    pub(super) highlight_batches: Vec<Batch>,
    /// Where the always-on-top highlights start in `highlight_batches`.
    pub(super) on_top_from: usize,
    /// The highlights drawn (at most [`MAX_HIGHLIGHTS`]).
    pub(super) highlights: usize,
    /// Each shadow cascade's batches, in `cascade_batches`.
    pub(super) cascades: Vec<Range<usize>>,
    pub(super) cascade_batches: Vec<Batch>,
    /// The cascade matrices' bind group, used at each cascade's offset.
    pub(super) cascade_group: Option<wgpu::BindGroup>,
    /// The distance between two cascades' uniforms.
    pub(super) cascade_slot: u64,
    /// Sprite and ribbon quads.
    pub(super) effect_quads: usize,
    /// Debug line vertices.
    pub(super) line_vertices: usize,
    /// What the frame draws of the instance sets (ADR-0052).
    pub(crate) sets: instances::SetFrame,

    // Working lists, kept for their allocations.
    draws: Vec<Draw>,
    /// Each draw's palette start; `None` draws unskinned.
    palettes: Vec<Option<u32>>,
    /// The stale handles reported this frame: kind (0 mesh, 1 material, 2
    /// texture, 3 instance set), index and generation.
    reported: HashSet<(u8, u32, u32)>,
    /// Each highlighted entity's highlight number (1-based; the first wins).
    highlight_of: HashMap<u64, u8>,
    /// Highlighted draws: on top, highlight number, `SKINNED` or 0, mesh index
    /// and draw.
    highlighted: Vec<(bool, u8, usize, u32, usize)>,
    opaque: Vec<(Key, usize)>,
    /// Blended draws and depth-tested quads: distance, kind (0 draw, 1 quad),
    /// draw or quad, and key.
    blended: Vec<(f32, u8, usize, Key)>,
    /// Shadow casters and their world bounds (`None`: cast into every cascade).
    casters: Vec<(usize, u32, usize, Option<Aabb>)>,
    light_boxes: Vec<Option<Aabb>>,
    /// Each cascade's light matrix and texel size.
    cascade_fits: Vec<(Mat4, f32)>,
    instances: Vec<InstanceRaw>,
    quad_uniforms: Vec<QuadUniform>,
    /// Each world quad's texture.
    quad_textures: Vec<Slot>,
    lines: Vec<LineVertex>,
    bytes: Vec<u8>,
}

impl PreparedFrame {
    /// Empties every list, keeping their allocations, and lets go of the
    /// frame's bind groups.
    pub(super) fn clear(&mut self) {
        self.batches.clear();
        self.opaque_batches = 0;
        self.palette = None;
        self.sky = false;
        self.sun_uv = None;
        self.tested_quads.clear();
        self.top_quads.clear();
        self.quad_groups.clear();
        self.highlight_batches.clear();
        self.on_top_from = 0;
        self.highlights = 0;
        self.cascades.clear();
        self.cascade_batches.clear();
        self.cascade_group = None;
        self.effect_quads = 0;
        self.line_vertices = 0;
        self.sets.clear();
        self.draws.clear();
        self.palettes.clear();
        self.reported.clear();
        self.highlight_of.clear();
        self.highlighted.clear();
        self.opaque.clear();
        self.blended.clear();
        self.casters.clear();
        self.light_boxes.clear();
        self.cascade_fits.clear();
        self.instances.clear();
        self.quad_uniforms.clear();
        self.quad_textures.clear();
        self.lines.clear();
        self.bytes.clear();
    }
}

/// Fills `bytes` with one uniform slot of `slot` bytes per value, each value
/// at the start of its slot.
fn slotted<T: bytemuck::Pod>(
    bytes: &mut Vec<u8>,
    slot: u64,
    values: impl ExactSizeIterator<Item = T>,
) {
    bytes.clear();
    bytes.resize(slot as usize * values.len(), 0);
    for (n, v) in values.enumerate() {
        let at = n * slot as usize;
        bytes[at..at + size_of::<T>()].copy_from_slice(bytemuck::bytes_of(&v));
    }
}

/// Bind groups kept by their inputs, so a frame makes one only when an input
/// changed.
#[derive(Default)]
pub(super) struct BindGroups {
    /// The cascade matrices' group, for the buffer it binds.
    cascade: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    /// The quad uniform buffer the quad groups bind.
    quad_buffer: Option<wgpu::Buffer>,
    /// The world quad textures' groups by texture index, each with its
    /// texture's generation.
    quads: Vec<Option<(u32, wgpu::BindGroup)>>,
}

impl BindGroups {
    /// Forgets the quad group of the texture at `index`, released, so the
    /// group does not keep the texture's memory.
    pub(super) fn forget_texture(&mut self, index: u32) {
        if let Some(group) = self.quads.get_mut(index as usize) {
            *group = None;
        }
    }
}

impl Skinning {
    /// The joint palette's bind group over its first `size` bytes, made again
    /// only when the size or the buffer changed.
    fn palette_group(&mut self, device: &wgpu::Device, size: u64) -> wgpu::BindGroup {
        if let Some((buffer, kept, group)) = &self.palette_group
            && *buffer == self.palette.buffer
            && *kept == size
        {
            return group.clone();
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("re_render joint palette"),
            layout: &self.palette_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                // Only the frame's palette, so the binding stays within the
                // storage binding limit whatever the buffer grew to.
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &self.palette.buffer,
                    offset: 0,
                    size: NonZeroU64::new(size),
                }),
            }],
        });
        self.palette_group = Some((self.palette.buffer.clone(), size, group.clone()));
        group
    }
}

impl Renderer {
    /// Prepares `snapshot` with `extras` for drawing into a target of `size`
    /// seen from `view`: fills `prepared`, writes the frame's buffers and
    /// makes the pipelines and targets the passes need.
    #[expect(
        clippy::too_many_arguments,
        reason = "the frame's inputs, borrowed apart"
    )]
    pub(super) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        prepared: &mut PreparedFrame,
        view: &View,
        size: (u32, u32),
        snapshot: &RenderSnapshot,
        extras: &SceneExtras,
        report: &mut FrameReport,
    ) {
        prepared.clear();
        prepared.size = size;
        let camera_pos = view.camera_pos;
        let shadows = frame::sanitize_shadows(&snapshot.shadows);
        let layers = u32::from(shadows.cascades);
        let map_size = self
            .settings
            .shadow_map_size
            .clamp(1, device.limits().max_texture_dimension_2d);
        if self.shadow.size != map_size || self.shadow.layers != layers {
            self.shadow = Self::create_shadow_map(
                device,
                &self.layouts,
                &self.frame_buffer,
                &self.shadow_sampler,
                map_size,
                layers,
            );
        }
        let frustum = frame::Frustum::new(&view.view_proj);
        let samples = self.sample_count();
        prepared.samples = samples;
        report.stats.samples = samples;

        if extras.highlights.len() > MAX_HIGHLIGHTS {
            report.warnings.push(
                Diagnostic::warning(
                    codes::TOO_MANY_HIGHLIGHTS,
                    format!(
                        "{} highlights in the frame; only the first {MAX_HIGHLIGHTS} are drawn",
                        extras.highlights.len()
                    ),
                )
                .with_hint("remove highlights that are not needed, or group objects under one"),
            );
        }
        prepared.highlights = extras.highlights.len().min(MAX_HIGHLIGHTS);
        let drawn_highlights = &extras.highlights[..prepared.highlights];
        for (n, h) in drawn_highlights.iter().enumerate() {
            for e in &h.entities {
                prepared.highlight_of.entry(e.0).or_insert(n as u8 + 1);
            }
        }

        self.resolve_draws(
            device, prepared, snapshot, &frustum, camera_pos, extras, report,
        );
        self.world_quads(prepared, snapshot, camera_pos, report);
        batch(prepared);
        self.sets.prepare(
            &mut prepared.sets,
            snapshot,
            &self.meshes,
            &self.materials,
            &mut prepared.reported,
            &frustum,
            camera_pos,
            report,
        );

        // Lights and the shadow fit.
        let exposure = Some(snapshot.post.exposure)
            .filter(|e| e.is_finite() && *e >= 0.0)
            .unwrap_or(1.0);
        let lights = frame::select_lights(&snapshot.lights, camera_pos, exposure);
        report.warnings.extend(lights.warnings);
        report.stats.point_lights = lights.points.len() as u32;
        let shadow_far = shadows
            .distance
            .min(snapshot.camera.far)
            .max(snapshot.camera.near * 2.0);
        let splits = frame::cascade_splits(
            snapshot.camera.near,
            shadow_far,
            usize::from(shadows.cascades),
            shadows.split_lambda,
        );
        if let Some(sun) = lights
            .sun
            .filter(|s| s.shadows && (!prepared.casters.is_empty() || prepared.sets.has_casters()))
        {
            let light = frame::light_view(sun.direction);
            prepared.light_boxes.extend(
                prepared
                    .casters
                    .iter()
                    .map(|c| c.3.map(|b| frame::transform_aabb(&b, &light))),
            );
            prepared.sets.light_boxes(&light);
            let mut near = snapshot.camera.near;
            for &far in &splits[..usize::from(shadows.cascades)] {
                let fit = frame::fit_cascade(
                    &light,
                    &snapshot.camera,
                    &view.projection,
                    near,
                    far,
                    self.shadow.size,
                );
                near = far;
                // The nearest depth along the light of the cascade's casters.
                let mut caster_depth: Option<f32> = None;
                let start = prepared.cascade_batches.len();
                for (&(skinned, _, i, _), light_box) in
                    prepared.casters.iter().zip(&prepared.light_boxes)
                {
                    if let Some(b) = light_box {
                        if !fit.takes_caster(b) {
                            continue;
                        }
                        caster_depth = Some(caster_depth.map_or(-b.max.z, |d| d.min(-b.max.z)));
                    }
                    let item = &prepared.draws[i];
                    prepared.instances.push(
                        InstanceRaw::new(item.world, false, false)
                            .with_palette(prepared.palettes[i]),
                    );
                    let mesh = (item.mesh.index, item.mesh.generation);
                    match prepared.cascade_batches[start..].last_mut() {
                        Some(b) if b.variant == skinned && b.mesh == mesh => b.instances += 1,
                        _ => prepared.cascade_batches.push(Batch {
                            variant: skinned,
                            material: (0, 0),
                            mesh,
                            first_instance: prepared.instances.len() as u32 - 1,
                            instances: 1,
                        }),
                    }
                }
                // The instance sets' casters, in runs of their own.
                if let Some(d) = prepared.sets.cascade(&fit) {
                    caster_depth = Some(caster_depth.map_or(d, |c| c.min(d)));
                }
                prepared
                    .cascade_fits
                    .push((fit.light_view_proj(&light, caster_depth), fit.texel_world));
                prepared
                    .cascades
                    .push(start..prepared.cascade_batches.len());
            }
        }
        let shadowed = !prepared.cascades.is_empty();
        report.stats.shadow_cascades = prepared.cascades.len() as u32;
        let mut cascade_matrices = [Mat4::IDENTITY.to_cols_array_2d(); frame::MAX_CASCADES];
        let mut cascade_offset = [0.0; 4];
        for (n, (m, texel)) in prepared.cascade_fits.iter().enumerate() {
            cascade_matrices[n] = m.to_cols_array_2d();
            cascade_offset[n] = texel * 1.5;
        }
        let mut points = [PointLightRaw::default(); frame::MAX_POINT_LIGHTS];
        points[..lights.points.len()].copy_from_slice(&lights.points);
        let spot_list = frame::select_spots(
            &extras.spot_lights,
            camera_pos,
            exposure,
            &mut report.warnings,
        );
        report.stats.spot_lights = spot_list.len() as u32;
        let mut spots = [SpotLightRaw::default(); extras::MAX_SPOT_LIGHTS];
        spots[..spot_list.len()].copy_from_slice(&spot_list);
        let (ambient_sky, ambient_ground) = extras.ambient.map_or(
            (self.settings.ambient_sky, self.settings.ambient_ground),
            |a| (a.sky, a.ground),
        );
        let fog = extras.fog.filter(|f| {
            f.start.is_finite() && f.end.is_finite() && f.color.iter().all(|c| c.is_finite())
        });
        let sky = extras.sky;
        let sky_sun = sky.and_then(|s| s.sun).filter(|(d, c)| {
            d.is_finite() && d.length_squared() > 1e-12 && c.iter().all(|v| v.is_finite())
        });
        prepared.sky = sky.is_some();
        let view_proj = view.view_proj;
        let uniform = FrameUniform {
            inv_view_proj: view_proj.inverse().to_cols_array_2d(),
            fog_color: fog.map_or([0.0; 4], |f| frame::rgb_w(f.color, f.start)),
            fog_params: [
                fog.map_or(0.0, |f| f.end),
                f32::from(u8::from(fog.is_some())),
                spot_list.len() as f32,
                0.0,
            ],
            sky_zenith: sky.map_or([0.0; 4], |s| frame::rgb_w(s.zenith, 1.0)),
            sky_horizon: sky.map_or([0.0; 4], |s| frame::rgb_w(s.horizon, 0.0)),
            sky_ground: sky.map_or([0.0; 4], |s| frame::rgb_w(s.ground, 0.0)),
            sky_sun_dir: sky_sun.map_or([0.0; 4], |(d, _)| d.normalize().extend(1.0).to_array()),
            sky_sun_color: sky_sun.map_or([0.0; 4], |(_, c)| frame::rgb_w(c, 0.0)),
            spots,
            view_proj: view_proj.to_cols_array_2d(),
            cascades: cascade_matrices,
            cascade_far: splits,
            cascade_offset,
            shadow_params: [prepared.cascades.len() as f32, shadows.blend, 0.0, 0.0],
            camera_pos: camera_pos.extend(1.0).to_array(),
            camera_forward: (-snapshot.camera.world.z_axis.truncate())
                .normalize_or_zero()
                .extend(0.0)
                .to_array(),
            sun_dir: lights
                .sun
                .map_or([0.0; 4], |s| (-s.direction).extend(1.0).to_array()),
            sun_color: lights.sun.map_or([0.0; 4], |s| {
                s.radiance.extend(f32::from(u8::from(shadowed))).to_array()
            }),
            ambient_sky: frame::rgb_w((Vec3::from_array(ambient_sky) * exposure).to_array(), 0.0),
            ambient_ground: frame::rgb_w(
                (Vec3::from_array(ambient_ground) * exposure).to_array(),
                0.0,
            ),
            params: [
                1.0 / self.shadow.size as f32,
                0.0,
                lights.points.len() as f32,
                exposure,
            ],
            points,
        };
        queue.write_buffer(&self.frame_buffer, 0, bytemuck::bytes_of(&uniform));

        // The sun's place on screen, for sun rays: the sky's sun disc, else the
        // sun light.
        let to_sun = sky_sun
            .map(|(d, _)| d.normalize())
            .or(lights.sun.map(|s| -s.direction));
        prepared.sun_uv = to_sun.and_then(|d| {
            let clip = view_proj * Vec4::from((d, 0.0));
            (clip.w > 1e-6).then(|| {
                let ndc = clip.truncate().truncate() / clip.w;
                Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
            })
        });

        self.upload(
            device, queue, prepared, snapshot, extras, camera_pos, report,
        );
        self.pipelines_for(samples);
        self.post.prepare_targets(device, size, samples);
    }

    /// Resolves every draw item and skinned draw item, skipping (and reporting
    /// once) the ones with stale handles, culls them against `frustum` and
    /// sorts them into the opaque, blended, caster and highlighted lists.
    #[expect(
        clippy::too_many_arguments,
        reason = "the frame's inputs, borrowed apart"
    )]
    fn resolve_draws(
        &mut self,
        device: &wgpu::Device,
        prepared: &mut PreparedFrame,
        snapshot: &RenderSnapshot,
        frustum: &frame::Frustum,
        camera_pos: Vec3,
        extras: &SceneExtras,
        report: &mut FrameReport,
    ) {
        let drawn_highlights = &extras.highlights[..prepared.highlights];
        prepared.draws.extend(
            snapshot
                .items
                .iter()
                .map(|item| Draw {
                    entity: item.entity.0,
                    mesh: item.mesh,
                    material: item.material,
                    world: item.world,
                    flags: item.flags,
                    skinned: None,
                })
                .chain(snapshot.skinned.iter().map(|item| Draw {
                    entity: item.entity.0,
                    mesh: item.mesh,
                    material: item.material,
                    world: item.world,
                    flags: item.flags,
                    skinned: Some((item.palette, item.joints, item.bounds)),
                })),
        );
        // The palette the skinned items index, when the renderer can skin.
        let palette_limit = self.skinning.as_ref().map(|_| {
            let binding = device.limits().max_storage_buffer_binding_size;
            MAX_PALETTE.min((binding / size_of::<Mat4>() as u64) as usize)
        });
        let palette_len = snapshot.joint_palette.len();
        prepared.palettes.resize(prepared.draws.len(), None);
        for (i, item) in prepared.draws.iter().enumerate() {
            let mesh = self.meshes.get(item.mesh.index, item.mesh.generation);
            let material = self
                .materials
                .get(item.material.index, item.material.generation);
            let (Some(mesh), Some(material)) = (mesh, material) else {
                report.stats.items_skipped += 1;
                if mesh.is_none()
                    && prepared
                        .reported
                        .insert((0, item.mesh.index, item.mesh.generation))
                {
                    report.warnings.push(stale_item(
                        codes::STALE_MESH,
                        "mesh",
                        item.mesh.index,
                        item.mesh.generation,
                        item.entity,
                    ));
                }
                if material.is_none()
                    && prepared
                        .reported
                        .insert((1, item.material.index, item.material.generation))
                {
                    report.warnings.push(stale_item(
                        codes::STALE_MATERIAL,
                        "material",
                        item.material.index,
                        item.material.generation,
                        item.entity,
                    ));
                }
                continue;
            };
            let mut world_box = mesh.aabb.map(|b| frame::transform_aabb(&b, &item.world));
            if let Some((first, joints, bounds)) = item.skinned {
                match palette_limit {
                    None if !self.warned_no_skinning => {
                        self.warned_no_skinning = true;
                        report.warnings.push(
                            Diagnostic::warning(
                                codes::SKINNING_UNSUPPORTED,
                                "this adapter cannot read storage buffers in vertex shaders; \
                                 skinned meshes are drawn in their bind pose",
                            )
                            .with_hint("use Vulkan, Metal, DirectX 12 or desktop GL 4.3+"),
                        );
                    }
                    None => {}
                    Some(limit) => {
                        let end = u64::from(first) + u64::from(joints);
                        let problem = match &mesh.skin {
                            None => Some(
                                "its mesh has no skin (upload it with upload_skinned_mesh)"
                                    .to_string(),
                            ),
                            Some(_) if palette_len > limit => Some(format!(
                                "the joint palette has {palette_len} matrices, more than \
                                 the {limit} this renderer can bind"
                            )),
                            Some(_) if end > palette_len as u64 => Some(format!(
                                "it uses palette {first}..{end}, but the joint palette has \
                                 {palette_len} matrices"
                            )),
                            Some(skin) if joints < skin.joints_used => Some(format!(
                                "it has {joints} joints, but its mesh's weights use {}",
                                skin.joints_used
                            )),
                            Some(_) => None,
                        };
                        if let Some(problem) = problem {
                            report.stats.items_skipped += 1;
                            report.warnings.push(Diagnostic::warning(
                                codes::BAD_SKINNED_ITEM,
                                format!(
                                    "skinned item of entity {} is not drawn: {problem}",
                                    item.entity
                                ),
                            ));
                            continue;
                        }
                        prepared.palettes[i] = Some(first);
                        let finite = bounds.min.is_finite() && bounds.max.is_finite();
                        world_box = finite.then_some(bounds);
                    }
                }
                report.stats.skinned_items += u32::from(prepared.palettes[i].is_some());
            }
            let in_view = world_box.is_none_or(|b| frustum.intersects(&b));
            if in_view {
                report.stats.items_drawn += 1;
            } else {
                report.stats.items_culled += 1;
            }
            if mesh.index_count == 0 {
                continue;
            }
            let skinned = if prepared.palettes[i].is_some() {
                SKINNED
            } else {
                0
            };
            if item.flags.contains(DrawFlags::CAST_SHADOWS) {
                prepared
                    .casters
                    .push((skinned, item.mesh.index, i, world_box));
            }
            if let Some(&n) = prepared.highlight_of.get(&item.entity) {
                let on_top = drawn_highlights[usize::from(n - 1)].always_on_top;
                prepared
                    .highlighted
                    .push((on_top, n, skinned, item.mesh.index, i));
            }
            let key = (
                material.variant() | skinned,
                item.material.index,
                item.mesh.index,
            );
            if !in_view {
                continue;
            }
            if material.blend {
                let depth = world_box
                    .map_or(item.world.w_axis.truncate(), |b| b.center())
                    .distance_squared(camera_pos);
                prepared.blended.push((depth, 0u8, i, key));
            } else {
                prepared.opaque.push((key, i));
            }
        }
    }

    /// The world quads: each one's uniform and texture; the depth-tested ones
    /// are sorted with the blended items.
    fn world_quads(
        &self,
        prepared: &mut PreparedFrame,
        snapshot: &RenderSnapshot,
        camera_pos: Vec3,
        report: &mut FrameReport,
    ) {
        for q in &snapshot.quads {
            if self
                .textures
                .get(q.texture.index, q.texture.generation)
                .is_none()
            {
                report.stats.items_skipped += 1;
                if prepared
                    .reported
                    .insert((2, q.texture.index, q.texture.generation))
                {
                    report.warnings.push(stale_item(
                        codes::STALE_TEXTURE,
                        "texture",
                        q.texture.index,
                        q.texture.generation,
                        q.entity.0,
                    ));
                }
                continue;
            }
            let bias = match q.depth {
                QuadDepth::Tested { bias } => Some(bias),
                QuadDepth::OnTop => None,
            };
            let values = [q.light_influence, q.brightness, bias.unwrap_or(0.0)];
            if !q.world.is_finite() || !q.size.is_finite() || values.iter().any(|v| !v.is_finite())
            {
                continue;
            }
            let center = q.world.w_axis.truncate();
            let towards_camera = (camera_pos - center).normalize_or_zero() * bias.unwrap_or(0.0);
            let world = Mat4::from_translation(towards_camera)
                * q.world
                * Mat4::from_scale(q.size.extend(1.0));
            let n = prepared.quad_uniforms.len();
            prepared.quad_uniforms.push(QuadUniform {
                world: world.to_cols_array_2d(),
                params: [
                    q.light_influence,
                    q.brightness,
                    f32::from(u8::from(bias.is_some())),
                    0.0,
                ],
            });
            prepared
                .quad_textures
                .push((q.texture.index, q.texture.generation));
            if bias.is_some() {
                let depth = (center + towards_camera).distance_squared(camera_pos);
                prepared.blended.push((depth, 1u8, n, (0, 0, 0)));
            } else {
                prepared.top_quads.push(n);
            }
        }
        report.stats.world_quads = prepared.quad_uniforms.len() as u32;
    }

    /// Writes the instances, the instance sets' entries, the joint palette,
    /// the debug lines, the sprites and ribbons, the cascades' and the world
    /// quads' uniforms, and finds the bind groups that read them.
    #[expect(
        clippy::too_many_arguments,
        reason = "the frame's inputs, borrowed apart"
    )]
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        prepared: &mut PreparedFrame,
        snapshot: &RenderSnapshot,
        extras: &SceneExtras,
        camera_pos: Vec3,
        report: &mut FrameReport,
    ) {
        self.instances
            .write(device, queue, bytemuck::cast_slice(&prepared.instances));
        self.sets.write(device, queue, &mut prepared.sets);
        prepared.palette = match &mut self.skinning {
            Some(skinning) if prepared.palettes.iter().any(Option::is_some) => {
                skinning.palette.write(
                    device,
                    queue,
                    bytemuck::cast_slice(&snapshot.joint_palette),
                );
                let bytes = snapshot.joint_palette.len().max(1) * size_of::<Mat4>();
                Some(skinning.palette_group(device, bytes as u64))
            }
            _ => None,
        };
        prepared
            .lines
            .extend(snapshot.debug_lines.iter().flat_map(|l| {
                [
                    LineVertex {
                        position: l.a.to_array(),
                        color: l.color,
                    },
                    LineVertex {
                        position: l.b.to_array(),
                        color: l.color,
                    },
                ]
            }));
        self.lines
            .write(device, queue, bytemuck::cast_slice(&prepared.lines));
        prepared.line_vertices = prepared.lines.len();
        report.stats.debug_lines = snapshot.debug_lines.len() as u32;
        let effect_vertices = extras::effect_quads(
            &extras.sprites,
            &extras.ribbons,
            camera_pos,
            snapshot.camera.world.x_axis.truncate().normalize_or_zero(),
            snapshot.camera.world.y_axis.truncate().normalize_or_zero(),
        );
        let quads = effect_vertices.len() / 4;
        prepared.effect_quads = quads;
        report.stats.effect_quads = quads as u32;
        if quads > 0 {
            self.effect_vertices
                .write(device, queue, bytemuck::cast_slice(&effect_vertices));
            // A quad's indices depend only on its place, so they are written
            // when more quads than ever are drawn, for the next power of two.
            if quads > self.effect_index_quads {
                let indices = quads.next_power_of_two();
                self.effect_indices.write(
                    device,
                    queue,
                    bytemuck::cast_slice(&extras::quad_indices(indices)),
                );
                self.effect_index_quads = indices;
            }
        }

        // One uniform slot per cascade, its matrix at a dynamic offset.
        prepared.cascade_slot = (size_of::<CascadeUniform>() as u64).next_multiple_of(u64::from(
            device.limits().min_uniform_buffer_offset_alignment,
        ));
        if !prepared.cascades.is_empty() {
            slotted(
                &mut prepared.bytes,
                prepared.cascade_slot,
                prepared.cascade_fits.iter().map(|(m, _)| CascadeUniform {
                    light_view_proj: m.to_cols_array_2d(),
                }),
            );
            self.cascade_uniforms.write(device, queue, &prepared.bytes);
            let buffer = &self.cascade_uniforms.buffer;
            prepared.cascade_group = Some(match &self.bind_groups.cascade {
                Some((kept, group)) if kept == buffer => group.clone(),
                _ => {
                    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("re_render cascade"),
                        layout: &self.layouts.cascade,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer,
                                offset: 0,
                                size: NonZeroU64::new(size_of::<CascadeUniform>() as u64),
                            }),
                        }],
                    });
                    self.bind_groups.cascade = Some((buffer.clone(), group.clone()));
                    group
                }
            });
        }

        // One uniform slot and bind group per world quad.
        prepared.quad_slot = (size_of::<QuadUniform>() as u64).next_multiple_of(u64::from(
            device.limits().min_uniform_buffer_offset_alignment,
        ));
        slotted(
            &mut prepared.bytes,
            prepared.quad_slot,
            prepared.quad_uniforms.iter().copied(),
        );
        self.quad_uniforms.write(device, queue, &prepared.bytes);
        let buffer = &self.quad_uniforms.buffer;
        let groups = &mut self.bind_groups;
        if groups.quad_buffer.as_ref() != Some(buffer) {
            groups.quads.clear();
            groups.quad_buffer = Some(buffer.clone());
        }
        for &(index, generation) in &prepared.quad_textures {
            let at = index as usize;
            if groups.quads.len() <= at {
                groups.quads.resize(at + 1, None);
            }
            let group = match &groups.quads[at] {
                Some((kept, group)) if *kept == generation => group.clone(),
                _ => {
                    let texture = self
                        .textures
                        .get(index, generation)
                        .expect("resolved when prepared");
                    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("re_render quad"),
                        layout: &self.layouts.quad,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                    buffer,
                                    offset: 0,
                                    size: NonZeroU64::new(size_of::<QuadUniform>() as u64),
                                }),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&texture.view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::Sampler(&self.quad_sampler),
                            },
                        ],
                    });
                    groups.quads[at] = Some((generation, group.clone()));
                    group
                }
            };
            prepared.quad_groups.push(group);
        }
    }
}

/// Sorts the scene's draws into batches by pipeline, material and mesh, the
/// blended ones back to front with the depth-tested quads among them, then
/// batches the highlighted items by mesh, the occluded ones first. Writes each
/// batch's instances.
fn batch(prepared: &mut PreparedFrame) {
    prepared.opaque.sort_by_key(|&(key, i)| (key, i));
    prepared
        .blended
        .sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    prepared
        .casters
        .sort_unstable_by_key(|&(skinned, mesh, i, _)| (skinned, mesh, i));

    let opaque_count = prepared.opaque.len();
    let ordered = prepared.opaque.iter().map(|&(key, i)| (0, key, i)).chain(
        prepared
            .blended
            .iter()
            .map(|&(_, kind, i, key)| (kind, key, i)),
    );
    // A depth-tested quad ends the batch before it.
    let mut split = false;
    for (n, (kind, key, i)) in ordered.enumerate() {
        if n == opaque_count {
            prepared.opaque_batches = prepared.batches.len();
        }
        if kind == 1 {
            prepared.tested_quads.push((prepared.batches.len(), i));
            split = true;
            continue;
        }
        let item = &prepared.draws[i];
        prepared.instances.push(
            InstanceRaw::new(
                item.world,
                item.flags.contains(DrawFlags::RECEIVE_SHADOWS),
                item.flags.contains(DrawFlags::SELECTED),
            )
            .with_palette(prepared.palettes[i]),
        );
        let material = (item.material.index, item.material.generation);
        let mesh = (item.mesh.index, item.mesh.generation);
        match prepared.batches.last_mut() {
            Some(b) if !split && b.variant == key.0 && b.material == material && b.mesh == mesh => {
                b.instances += 1
            }
            _ => prepared.batches.push(Batch {
                variant: key.0,
                material,
                mesh,
                first_instance: prepared.instances.len() as u32 - 1,
                instances: 1,
            }),
        }
        split = false;
    }
    if opaque_count == prepared.instances.len() {
        prepared.opaque_batches = prepared.batches.len();
    }

    // Highlighted items, the occluded ones first, batched by mesh.
    prepared.highlighted.sort_unstable();
    // The batch where the always-on-top ones start, once one is batched.
    let mut on_top_from = None;
    for &(on_top, n, skinned, _, i) in &prepared.highlighted {
        let item = &prepared.draws[i];
        let mut raw = InstanceRaw::new(item.world, false, false).with_palette(prepared.palettes[i]);
        raw.flags[2] = f32::from(n);
        prepared.instances.push(raw);
        let mesh = (item.mesh.index, item.mesh.generation);
        // The last batch is on top once the on-top ones have started.
        let same_kind = on_top_from.is_some() == on_top;
        match prepared.highlight_batches.last_mut() {
            Some(b) if same_kind && b.variant == skinned && b.mesh == mesh => b.instances += 1,
            _ => {
                if on_top && on_top_from.is_none() {
                    on_top_from = Some(prepared.highlight_batches.len());
                }
                prepared.highlight_batches.push(Batch {
                    variant: skinned,
                    material: (0, 0),
                    mesh,
                    first_instance: prepared.instances.len() as u32 - 1,
                    instances: 1,
                });
            }
        }
    }
    prepared.on_top_from = on_top_from.unwrap_or(prepared.highlight_batches.len());
}
