//! Instance sets (ADR-0052): many copies of a prefab's meshes, their
//! placements uploaded once and culled by cluster, so a frame's work grows
//! with its sets and clusters, never with their copies.
//!
//! A set is one vertex buffer of 32-byte placements, in its producer's cluster
//! order. Each frame, each part of each set in the snapshot is tested whole,
//! then cluster by cluster, against the view, the part's distance range and
//! each shadow cascade. Consecutive passing clusters are one instanced draw,
//! which binds the placement buffer at the run's offset and draws from
//! instance 0, so WebGL2, which has no base instance, draws them too. Each
//! (set, part) pair has one 256-byte uniform entry at a dynamic offset: the
//! set's and the part's matrices, their normal matrices and the flags
//! (`shaders/instances.wgsl`).
//!
//! The renderer calls in at the seams of its frame: [`Sets::prepare`] culls
//! once the items are batched, [`SetFrame::light_boxes`] and
//! [`SetFrame::cascade`] give each shadow cascade its casters, [`Sets::write`]
//! uploads the frame's entries, and the shadow, scene and highlight passes
//! record their runs with [`Sets::draw`].

mod cull;

use std::collections::HashSet;
use std::mem::size_of;
use std::num::NonZeroU64;
use std::ops::Range;

use glam::{Mat3, Mat4, Vec3};
use re_core::Diagnostic;
use re_types::mesh::Aabb;
use re_types::render::{DrawFlags, InstanceCluster, InstanceSetHandle, Placement, RenderSnapshot};

use crate::frame::{self, CascadeBox, Frustum};
use crate::renderer::{
    Drawn, Geometry, GpuMaterial, GpuMesh, GrowBuffer, Layouts, Slot, mask_pipelines,
    shadow_pipeline, stale,
};
use crate::slots::Slots;
use crate::{FrameReport, FrameStats, ResourceStats, codes, shader};
use cull::{Cluster, Distances, PartBoxes};

/// One placement as the GPU reads it: [`Placement`]'s layout, which `re_types`
/// cannot derive `Pod` for.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct PlacementRaw {
    position: [f32; 3],
    scale: f32,
    rotation: [i16; 4],
    tint: [u8; 4],
    id: u32,
}

impl From<&Placement> for PlacementRaw {
    fn from(p: &Placement) -> PlacementRaw {
        PlacementRaw {
            position: p.position,
            scale: p.scale,
            rotation: p.rotation,
            tint: p.tint,
            id: p.id,
        }
    }
}

/// The bytes of one placement in a set's buffer.
const PLACEMENT: u64 = size_of::<PlacementRaw>() as u64;

/// The placements' vertex buffer (slot 1, one placement per instance): the
/// position and scale at location 4, the rotation at 5 and the tint at 6. The
/// point id in the last 4 bytes is not read.
pub(crate) fn placement_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![4 => Float32x4, 5 => Snorm16x4, 6 => Unorm8x4];
    wgpu::VertexBufferLayout {
        array_stride: PLACEMENT,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRIBUTES,
    }
}

/// `struct SetPart` in instances.wgsl: one (set, part) pair's entry. It is 256
/// bytes, so entries side by side sit at every device's uniform offset
/// alignment.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct SetPartUniform {
    set: [[f32; 4]; 4],
    part: [[f32; 4]; 4],
    set_normal: [[f32; 4]; 3],
    part_normal: [[f32; 4]; 3],
    flags: [f32; 4],
    reserved: [f32; 4],
}

/// The bytes of one entry, and the distance between two.
const ENTRY: u64 = size_of::<SetPartUniform>() as u64;
const _: () = assert!(ENTRY == 256);

impl SetPartUniform {
    fn new(set: &Mat4, part: &Mat4, flags: DrawFlags) -> SetPartUniform {
        let flag = |f: DrawFlags| f32::from(u8::from(flags.contains(f)));
        SetPartUniform {
            set: set.to_cols_array_2d(),
            part: part.to_cols_array_2d(),
            set_normal: normal_matrix(set),
            part_normal: normal_matrix(part),
            flags: [
                flag(DrawFlags::RECEIVE_SHADOWS),
                flag(DrawFlags::SELECTED),
                0.0,
                0.0,
            ],
            reserved: [0.0; 4],
        }
    }
}

/// The inverse transpose of `m`'s upper 3x3, which keeps normals
/// perpendicular under non-uniform scale (a singular matrix is used as it is),
/// as three padded columns.
fn normal_matrix(m: &Mat4) -> [[f32; 4]; 3] {
    let linear = Mat3::from_mat4(*m);
    let normal = if linear.determinant().abs() > 1e-12 {
        linear.inverse().transpose()
    } else {
        linear
    };
    [
        normal.x_axis.extend(0.0).to_array(),
        normal.y_axis.extend(0.0).to_array(),
        normal.z_axis.extend(0.0).to_array(),
    ]
}

/// An uploaded set.
struct GpuSet {
    buffer: wgpu::Buffer,
    clusters: Vec<Cluster>,
    bytes: u64,
    /// Each part's cluster boxes, by the part's place in its set's `parts`.
    boxes: Vec<PartBoxes>,
}

/// One draw of a part of a set: a run of the set's consecutive clusters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    /// The part's forward pipeline (`double_sided | blend << 1`); 0 in the
    /// shadow cascades.
    pub(crate) variant: usize,
    pub(crate) material: Slot,
    pub(crate) mesh: Slot,
    pub(crate) set: Slot,
    /// The (set, part) pair's entry in the frame's entries.
    pub(crate) entry: u32,
    /// The run's placements.
    pub(crate) first: u32,
    pub(crate) count: u32,
}

/// What a frame draws of the instance sets, kept between frames for its
/// allocations.
#[derive(Default)]
pub(crate) struct SetFrame {
    /// The scene pass's runs: the opaque ones, then the blended ones from the
    /// farthest cluster to the nearest.
    pub(crate) runs: Vec<Run>,
    /// How many of `runs` are opaque.
    pub(crate) opaque_runs: usize,
    /// Each shadow cascade's runs, in `cascade_runs`.
    pub(crate) cascades: Vec<Range<usize>>,
    pub(crate) cascade_runs: Vec<Run>,
    /// The entries' bind group, used at each entry's offset.
    group: Option<wgpu::BindGroup>,
    entries: Vec<SetPartUniform>,
    /// One cluster of a casting part in its distance range, with its box.
    casters: Vec<(Run, Aabb)>,
    /// Each caster's box in the light's view.
    light_boxes: Vec<Aabb>,
    /// Blended parts' clusters: the squared distance to the box's centre, the
    /// order they were found in, and the cluster's run.
    blended: Vec<(f32, u32, Run)>,
}

/// What [`SetFrame::cull`] found for one part.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Culled {
    /// A cluster is drawn in the scene pass.
    drawn: bool,
    /// A cluster is drawn, or casts.
    used: bool,
}

impl SetFrame {
    /// Empties every list, keeping their allocations, and lets go of the bind
    /// group.
    pub(crate) fn clear(&mut self) {
        self.runs.clear();
        self.opaque_runs = 0;
        self.cascades.clear();
        self.cascade_runs.clear();
        self.group = None;
        self.entries.clear();
        self.casters.clear();
        self.light_boxes.clear();
        self.blended.clear();
    }

    /// True when a set casts a shadow this frame, so the sun's cascades are
    /// fitted.
    pub(crate) fn has_casters(&self) -> bool {
        !self.casters.is_empty()
    }

    /// Culls one part of a set, whose clusters are `clusters` with boxes
    /// `boxes`, into the scene pass's runs (copies of `run` for its
    /// placements) and, when it `casts`, the casters.
    #[expect(clippy::too_many_arguments, reason = "a part's inputs, borrowed apart")]
    fn cull(
        &mut self,
        clusters: &[Cluster],
        boxes: &PartBoxes,
        run: Run,
        distances: Distances,
        blended: bool,
        casts: bool,
        frustum: &Frustum,
        eye: Vec3,
        stats: &mut FrameStats,
    ) -> Culled {
        let whole = &boxes.whole;
        let mut drawn = 0;
        if frustum.intersects(whole) && distances.may_hold(whole, eye) {
            // The opaque run being extended.
            let mut open: Option<Run> = None;
            for (c, b) in clusters.iter().zip(&boxes.clusters) {
                if !(frustum.intersects(b) && distances.holds(b, eye)) {
                    stats.set_clusters_culled += 1;
                    continue;
                }
                drawn += 1;
                stats.set_instances += c.count;
                let one = Run {
                    first: c.first,
                    count: c.count,
                    ..run
                };
                if blended {
                    let depth = b.center().distance_squared(eye);
                    self.blended.push((depth, self.blended.len() as u32, one));
                    continue;
                }
                match &mut open {
                    Some(r) if r.first + r.count == c.first => r.count += c.count,
                    _ => self.runs.extend(open.replace(one)),
                }
            }
            self.runs.extend(open);
        } else {
            stats.set_clusters_culled += clusters.len() as u32;
        }
        stats.set_clusters += drawn;
        let before = self.casters.len();
        if casts && distances.may_hold(whole, eye) {
            for (c, b) in clusters.iter().zip(&boxes.clusters) {
                if distances.holds(b, eye) {
                    let one = Run {
                        variant: 0,
                        first: c.first,
                        count: c.count,
                        ..run
                    };
                    self.casters.push((one, *b));
                }
            }
        }
        Culled {
            drawn: drawn > 0,
            used: drawn > 0 || self.casters.len() > before,
        }
    }

    /// Puts the scene pass's runs in drawing order: the opaque ones by
    /// pipeline, material and mesh (keeping the snapshot's order among
    /// equals), then the blended ones from the farthest cluster to the
    /// nearest.
    fn finish(&mut self) {
        self.runs.sort_by_key(|r| (r.variant, r.material, r.mesh));
        self.opaque_runs = self.runs.len();
        self.blended
            .sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        self.runs
            .extend(self.blended.iter().map(|&(_, _, run)| run));
    }

    /// Moves the casters' boxes into the light's view `light`, for
    /// [`SetFrame::cascade`].
    pub(crate) fn light_boxes(&mut self, light: &Mat4) {
        self.light_boxes.clear();
        self.light_boxes.extend(
            self.casters
                .iter()
                .map(|(_, b)| frame::transform_aabb(b, light)),
        );
    }

    /// Gives the next cascade the runs of the casters `fit` takes, and returns
    /// the nearest depth along the light of the ones it took.
    pub(crate) fn cascade(&mut self, fit: &CascadeBox) -> Option<f32> {
        let start = self.cascade_runs.len();
        let mut depth: Option<f32> = None;
        for (&(caster, _), b) in self.casters.iter().zip(&self.light_boxes) {
            if !fit.takes_caster(b) {
                continue;
            }
            depth = Some(depth.map_or(-b.max.z, |d| d.min(-b.max.z)));
            match self.cascade_runs[start..].last_mut() {
                Some(r) if r.entry == caster.entry && r.first + r.count == caster.first => {
                    r.count += caster.count
                }
                _ => self.cascade_runs.push(caster),
            }
        }
        self.cascades.push(start..self.cascade_runs.len());
        depth
    }
}

/// The uploaded instance sets, and what drawing them needs.
pub(crate) struct Sets {
    sets: Slots<GpuSet>,
    /// The frame's (set, part) entries, read at dynamic offsets.
    entries: GrowBuffer,
    /// The entries' bind group, for the buffer it binds.
    group: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    /// One entry at a dynamic offset, in the vertex stage.
    entry_layout: wgpu::BindGroupLayout,
    /// frame, material, entry.
    pub(crate) forward_layout: wgpu::PipelineLayout,
    pub(crate) forward_shader: wgpu::ShaderModule,
    /// cascade, entry: kept to rebuild `shadow_pipeline`.
    pub(crate) shadow_layout: wgpu::PipelineLayout,
    pub(crate) shadow_pipeline: wgpu::RenderPipeline,
    /// frame, entry: kept to rebuild `mask_pipelines`.
    pub(crate) mask_layout: wgpu::PipelineLayout,
    /// The highlight mask's depth-only (0) and numbering (1) pipelines; the
    /// mask draws sets only in its depth pass.
    pub(crate) mask_pipelines: [wgpu::RenderPipeline; 2],
}

impl Sets {
    /// No sets yet, with the pipelines that draw them into the shadow
    /// cascades and the highlight mask (the forward ones are among the
    /// renderer's scene pipelines).
    pub(crate) fn new(device: &wgpu::Device, layouts: &Layouts) -> Sets {
        let entry_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("re_render instance set part"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(ENTRY),
                },
                count: None,
            }],
        });
        let layout = |label: &str, groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: groups,
                immediate_size: 0,
            })
        };
        let forward_layout = layout(
            "re_render forward set",
            &[
                Some(&layouts.frame),
                Some(&layouts.material),
                Some(&entry_layout),
            ],
        );
        let shadow_layout = layout(
            "re_render shadow set",
            &[Some(&layouts.cascade), Some(&entry_layout)],
        );
        let mask_layout = layout(
            "re_render highlight mask set",
            &[Some(&layouts.shadow_frame), Some(&entry_layout)],
        );
        Sets {
            sets: Slots::default(),
            entries: GrowBuffer::new(
                device,
                "re_render instance set parts",
                wgpu::BufferUsages::UNIFORM,
            ),
            group: None,
            forward_shader: shader::module(device, "forward_set.wgsl"),
            shadow_pipeline: shadow_pipeline(
                device,
                &shadow_layout,
                &shader::module(device, "shadow_set.wgsl"),
                Geometry::Sets,
            ),
            mask_pipelines: mask_pipelines(
                device,
                &mask_layout,
                &shader::module(device, "mask_set.wgsl"),
                Geometry::Sets,
            ),
            entry_layout,
            forward_layout,
            shadow_layout,
            mask_layout,
        }
    }

    /// Uploads a set (`Renderer::upload_instance_set`).
    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        placements: &[Placement],
        clusters: &[InstanceCluster],
    ) -> Result<InstanceSetHandle, Diagnostic> {
        check(placements, clusters, device.limits().max_buffer_size)?;
        let raw: Vec<PlacementRaw> = placements.iter().map(PlacementRaw::from).collect();
        let bytes: &[u8] = bytemuck::cast_slice(&raw);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("re_render instance set"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, bytes);
        let mut first = 0;
        let clusters = clusters
            .iter()
            .map(|c| {
                let cluster = Cluster {
                    first,
                    count: c.count,
                    bounds: c.bounds,
                    max_scale: c.max_scale,
                };
                first += c.count;
                cluster
            })
            .collect();
        let (index, generation) = self.sets.insert(GpuSet {
            bytes: buffer.size(),
            buffer,
            clusters,
            boxes: Vec::new(),
        });
        Ok(InstanceSetHandle::new(index, generation))
    }

    /// Releases a set (`Renderer::release_instance_set`).
    pub(crate) fn release(&mut self, handle: InstanceSetHandle) -> Result<(), Diagnostic> {
        // Dropping the buffer frees it once the GPU is done with any frame
        // that still uses it.
        self.sets
            .remove(handle.index, handle.generation)
            .map(drop)
            .ok_or_else(|| {
                stale(
                    codes::STALE_INSTANCE_SET,
                    "instance set",
                    handle.index,
                    handle.generation,
                )
            })
    }

    /// The live sets and their GPU bytes.
    pub(crate) fn stats(&self) -> ResourceStats {
        ResourceStats {
            count: self.sets.len(),
            bytes: self.sets.values().map(|s| s.bytes).sum(),
        }
    }

    /// Culls the snapshot's sets into `frame`: each part's clusters against
    /// `frustum` and the part's distance range from `eye` into the scene
    /// pass's runs, and a casting part's clusters in its range into the
    /// casters that [`SetFrame::cascade`] sorts into cascades. A stale set,
    /// mesh or material handle skips its set or part, counted in
    /// `items_skipped` and warned about once a frame through `reported`,
    /// which the items share (kinds 0 mesh, 1 material, 3 set).
    #[expect(
        clippy::too_many_arguments,
        reason = "the frame's inputs, borrowed apart"
    )]
    pub(crate) fn prepare(
        &mut self,
        frame: &mut SetFrame,
        snapshot: &RenderSnapshot,
        meshes: &Slots<GpuMesh>,
        materials: &Slots<GpuMaterial>,
        reported: &mut HashSet<(u8, u32, u32)>,
        frustum: &Frustum,
        eye: Vec3,
        report: &mut FrameReport,
    ) {
        for set in &snapshot.instance_sets {
            let handle = (set.set.index, set.set.generation);
            let Some(gpu) = self.sets.get_mut(handle.0, handle.1) else {
                report.stats.items_skipped += 1;
                if reported.insert((3, handle.0, handle.1)) {
                    report.warnings.push(stale_in_set(
                        codes::STALE_INSTANCE_SET,
                        "instance set",
                        handle,
                        set.entity.0,
                        "the set",
                    ));
                }
                continue;
            };
            gpu.boxes.resize_with(set.parts.len(), PartBoxes::default);
            let mut drawn = false;
            for (part, boxes) in set.parts.iter().zip(&mut gpu.boxes) {
                let mesh_slot = (part.mesh.index, part.mesh.generation);
                let material_slot = (part.material.index, part.material.generation);
                let mesh = meshes.get(mesh_slot.0, mesh_slot.1);
                let material = materials.get(material_slot.0, material_slot.1);
                let (Some(mesh), Some(material)) = (mesh, material) else {
                    report.stats.items_skipped += 1;
                    if mesh.is_none() && reported.insert((0, mesh_slot.0, mesh_slot.1)) {
                        report.warnings.push(stale_in_set(
                            codes::STALE_MESH,
                            "mesh",
                            mesh_slot,
                            set.entity.0,
                            "the part",
                        ));
                    }
                    if material.is_none() && reported.insert((1, material_slot.0, material_slot.1))
                    {
                        report.warnings.push(stale_in_set(
                            codes::STALE_MATERIAL,
                            "material",
                            material_slot,
                            set.entity.0,
                            "the part",
                        ));
                    }
                    continue;
                };
                let Some(aabb) = mesh.aabb.filter(|_| mesh.index_count > 0) else {
                    continue;
                };
                // The source's and the part's shadow flags must both allow it.
                let both = |f: DrawFlags| set.flags.contains(f) && part.flags.contains(f);
                let mut flags = DrawFlags::NONE;
                for f in [DrawFlags::CAST_SHADOWS, DrawFlags::RECEIVE_SHADOWS] {
                    if both(f) {
                        flags |= f;
                    }
                }
                if set.flags.contains(DrawFlags::SELECTED)
                    || part.flags.contains(DrawFlags::SELECTED)
                {
                    flags |= DrawFlags::SELECTED;
                }
                let run = Run {
                    variant: material.variant(),
                    material: material_slot,
                    mesh: mesh_slot,
                    set: handle,
                    entry: frame.entries.len() as u32,
                    first: 0,
                    count: 0,
                };
                let boxes = boxes.update(&gpu.clusters, &set.transform, &part.transform, &aabb);
                let culled = frame.cull(
                    &gpu.clusters,
                    boxes,
                    run,
                    Distances::new(part.min_distance, part.max_distance),
                    material.blend,
                    flags.contains(DrawFlags::CAST_SHADOWS),
                    frustum,
                    eye,
                    &mut report.stats,
                );
                drawn |= culled.drawn;
                if culled.used {
                    frame
                        .entries
                        .push(SetPartUniform::new(&set.transform, &part.transform, flags));
                }
            }
            report.stats.instance_sets += u32::from(drawn);
        }
        frame.finish();
    }

    /// Writes the frame's entries and finds the bind group that reads them.
    pub(crate) fn write(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &mut SetFrame,
    ) {
        if frame.entries.is_empty() {
            return;
        }
        self.entries
            .write(device, queue, bytemuck::cast_slice(&frame.entries));
        let buffer = &self.entries.buffer;
        frame.group = Some(match &self.group {
            Some((kept, group)) if kept == buffer => group.clone(),
            _ => {
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("re_render instance set parts"),
                    layout: &self.entry_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer,
                            offset: 0,
                            size: NonZeroU64::new(ENTRY),
                        }),
                    }],
                });
                self.group = Some((buffer.clone(), group.clone()));
                group
            }
        });
    }

    /// Draws `runs` of `frame` into `pass`, with `pipeline`'s pipeline for
    /// each run's variant and its entry at `entry_group`; with `materials`,
    /// each run binds its material at the group given there. Like
    /// `Renderer::draw_batches`, only what changes from one run to the next
    /// is set.
    #[expect(clippy::too_many_arguments, reason = "a pass's inputs, borrowed apart")]
    pub(crate) fn draw<'p>(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        frame: &SetFrame,
        runs: &[Run],
        meshes: &Slots<GpuMesh>,
        materials: Option<(u32, &Slots<GpuMaterial>)>,
        entry_group: u32,
        pipeline: &dyn Fn(usize) -> &'p wgpu::RenderPipeline,
    ) -> Drawn {
        let mut drawn = Drawn::default();
        let Some(group) = frame.group.as_ref().filter(|_| !runs.is_empty()) else {
            return drawn;
        };
        // The variant, material, mesh and entry currently bound.
        let mut current: (Option<usize>, Option<Slot>, Option<Slot>, Option<u32>) =
            (None, None, None, None);
        for run in runs {
            if current.0 != Some(run.variant) {
                pass.set_pipeline(pipeline(run.variant));
                current.0 = Some(run.variant);
            }
            if let Some((at, materials)) = materials
                && current.1 != Some(run.material)
            {
                let material = materials
                    .get(run.material.0, run.material.1)
                    .expect("resolved when prepared");
                pass.set_bind_group(at, &material.bind_group, &[]);
                current.1 = Some(run.material);
            }
            if current.3 != Some(run.entry) {
                let offset = u64::from(run.entry) * ENTRY;
                pass.set_bind_group(entry_group, group, &[offset as u32]);
                current.3 = Some(run.entry);
            }
            let mesh = meshes
                .get(run.mesh.0, run.mesh.1)
                .expect("resolved when prepared");
            if current.2 != Some(run.mesh) {
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                current.2 = Some(run.mesh);
            }
            let set = self
                .sets
                .get(run.set.0, run.set.1)
                .expect("resolved when prepared");
            let start = u64::from(run.first) * PLACEMENT;
            pass.set_vertex_buffer(
                1,
                set.buffer
                    .slice(start..start + u64::from(run.count) * PLACEMENT),
            );
            pass.draw_indexed(0..mesh.index_count, 0, 0..run.count);
            drawn.draw_calls += 1;
            drawn.instances += run.count;
            drawn.triangles += u64::from(mesh.index_count / 3) * u64::from(run.count);
        }
        drawn
    }
}

/// Why `placements` and `clusters` cannot be uploaded as a set: `RE6101` when
/// the placements take more than `max_bytes` (the device's largest buffer),
/// `RE6100` when the set is malformed.
fn check(
    placements: &[Placement],
    clusters: &[InstanceCluster],
    max_bytes: u64,
) -> Result<(), Diagnostic> {
    let bad = |message: String| Diagnostic::error(codes::BAD_INSTANCE_SET, message);
    let len = placements.len();
    if len == 0 {
        return Err(bad("the instance set has no placements".to_string())
            .with_hint("make no set where nothing is placed"));
    }
    let bytes = len as u64 * PLACEMENT;
    if bytes > max_bytes || u32::try_from(len).is_err() {
        return Err(Diagnostic::error(
            codes::INSTANCE_SET_TOO_LARGE,
            format!(
                "the instance set's {len} placements take {bytes} bytes, more than this GPU's \
                 {max_bytes}-byte buffer limit ({} placements)",
                max_bytes / PLACEMENT
            ),
        )
        .with_hint("split the placements into several sets, such as one per streamed cell"));
    }
    if let Some(at) = placements
        .iter()
        .position(|p| !p.position.iter().all(|v| v.is_finite()) || !p.scale.is_finite())
    {
        return Err(bad(format!(
            "placement {at} has a position or scale that is not finite"
        )));
    }
    if let Some(at) = placements.iter().position(|p| p.rotation == [0; 4]) {
        return Err(bad(format!(
            "placement {at} has a zero rotation; a rotation is a unit quaternion, and no \
             rotation is [0, 0, 0, 32767]"
        )));
    }
    let held: u64 = clusters.iter().map(|c| u64::from(c.count)).sum();
    if held != len as u64 {
        return Err(bad(format!(
            "the {} clusters hold {held} placements, but the set has {len}",
            clusters.len()
        ))
        .with_hint("each placement belongs to exactly one cluster, in cluster order"));
    }
    let mut first = 0;
    for (n, c) in clusters.iter().enumerate() {
        if c.count == 0 {
            return Err(bad(format!("cluster {n} holds no placements")));
        }
        let finite =
            c.bounds.min.is_finite() && c.bounds.max.is_finite() && c.max_scale.is_finite();
        if !finite {
            return Err(bad(format!(
                "cluster {n} has a bound or largest scale that is not finite"
            )));
        }
        let end = first + c.count as usize;
        if let Some(at) = placements[first..end]
            .iter()
            .position(|p| !c.bounds.contains(Vec3::from(p.position)) || p.scale.abs() > c.max_scale)
        {
            return Err(bad(format!(
                "placement {} lies outside cluster {n}'s bounds, or its scale is larger than \
                 the cluster's largest",
                first + at
            ))
            .with_hint("make each cluster with InstanceCluster::of over its placements"));
        }
        first = end;
    }
    Ok(())
}

/// The warning for a set, or a part of one, whose `kind` handle is stale.
fn stale_in_set(
    code: re_core::Code,
    kind: &str,
    (index, generation): Slot,
    entity: u64,
    skipped: &str,
) -> Diagnostic {
    let mut d = stale(code, kind, index, generation);
    d.severity = re_core::Severity::Warning;
    d.message = format!(
        "an instance set of entity {entity} uses the {kind} handle {index}:{generation}, which \
         was released or never issued; {skipped} is skipped"
    );
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use re_types::render::Camera;

    fn placement(x: f32, z: f32) -> Placement {
        Placement::new(Vec3::new(x, 0.0, z), glam::Quat::IDENTITY, 1.0, 0)
    }

    /// `n` placements in a row along +x, 4 m apart, in clusters of `per`.
    fn row(n: usize, per: usize) -> (Vec<Placement>, Vec<InstanceCluster>) {
        let placements: Vec<Placement> = (0..n).map(|i| placement(i as f32 * 4.0, 0.0)).collect();
        let clusters = placements
            .chunks(per)
            .map(|c| InstanceCluster::of(c).unwrap())
            .collect();
        (placements, clusters)
    }

    fn code(r: Result<(), Diagnostic>) -> String {
        r.unwrap_err().code.to_string()
    }

    #[test]
    fn malformed_sets_are_refused() {
        let (placements, clusters) = row(8, 4);
        assert!(check(&placements, &clusters, u64::MAX).is_ok());
        assert_eq!(code(check(&[], &[], u64::MAX)), "RE6100");
        // More bytes than the device's largest buffer.
        assert_eq!(code(check(&placements, &clusters, 8 * 32 - 1)), "RE6101");
        assert!(check(&placements, &clusters, 8 * 32).is_ok());

        let refused = |edit: &dyn Fn(&mut Vec<Placement>, &mut Vec<InstanceCluster>)| {
            let (mut p, mut c) = row(8, 4);
            edit(&mut p, &mut c);
            check(&p, &c, u64::MAX).unwrap_err()
        };
        for (what, d) in [
            (
                "a NaN position",
                refused(&|p, _| p[3].position[1] = f32::NAN),
            ),
            (
                "an infinite scale",
                refused(&|p, _| p[0].scale = f32::INFINITY),
            ),
            ("a zero rotation", refused(&|p, _| p[5].rotation = [0; 4])),
            ("too few held", refused(&|_, c| c[1].count = 3)),
            ("too many held", refused(&|_, c| c[0].count = 5)),
            (
                "an empty cluster",
                refused(&|_, c| c.insert(1, InstanceCluster { count: 0, ..c[0] })),
            ),
            ("a NaN bound", refused(&|_, c| c[0].bounds.max.x = f32::NAN)),
            (
                "a placement outside",
                refused(&|p, _| p[6].position[2] = 1.0),
            ),
            (
                "a scale above the largest",
                refused(&|p, _| p[2].scale = 1.5),
            ),
        ] {
            assert_eq!(d.code.to_string(), "RE6100", "{what}: {}", d.message);
        }
        // A mirrored copy's scale counts by its magnitude.
        let (mut p, c) = row(8, 4);
        p[1].scale = -1.0;
        assert!(check(&p, &c, u64::MAX).is_ok());
    }

    fn view(eye: Vec3, target: Vec3, far: f32) -> (Frustum, Vec3) {
        let camera = Camera {
            world: glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y).inverse(),
            fov_y_radians: 1.0,
            near: 0.1,
            far,
        };
        let projection = frame::projection(&camera, 1.0).unwrap();
        (Frustum::new(&(projection * camera.view())), eye)
    }

    /// The clusters of `row(n, per)` and their boxes for a unit cube part.
    fn row_boxes(n: usize, per: usize) -> (Vec<Cluster>, PartBoxes) {
        let (placements, clusters) = row(n, per);
        let mut first = 0;
        let clusters: Vec<Cluster> = clusters
            .iter()
            .map(|c| {
                let k = Cluster {
                    first,
                    count: c.count,
                    bounds: c.bounds,
                    max_scale: c.max_scale,
                };
                first += c.count;
                k
            })
            .collect();
        assert_eq!(first as usize, placements.len());
        let mut boxes = PartBoxes::default();
        let unit = Aabb::new(Vec3::splat(-0.5), Vec3::splat(0.5));
        boxes.update(&clusters, &Mat4::IDENTITY, &Mat4::IDENTITY, &unit);
        (clusters, boxes)
    }

    fn template() -> Run {
        Run {
            variant: 0,
            material: (1, 0),
            mesh: (2, 0),
            set: (3, 0),
            entry: 0,
            first: 0,
            count: 0,
        }
    }

    #[test]
    fn consecutive_visible_clusters_are_one_run() {
        // Ten clusters of four along +x, 16 m each; the camera at x = 70
        // looks along +x and sees clusters 4 to 9.
        let (clusters, boxes) = row_boxes(40, 4);
        let (frustum, eye) = view(Vec3::new(70.0, 1.0, 0.0), Vec3::new(200.0, 1.0, 0.0), 500.0);
        let mut frame = SetFrame::default();
        let mut stats = FrameStats::default();
        let culled = frame.cull(
            &clusters,
            &boxes,
            template(),
            Distances::new(0.0, 0.0),
            false,
            true,
            &frustum,
            eye,
            &mut stats,
        );
        frame.finish();
        assert_eq!(
            culled,
            Culled {
                drawn: true,
                used: true
            }
        );
        assert_eq!(
            frame.runs,
            [Run {
                first: 16,
                count: 24,
                ..template()
            }]
        );
        assert_eq!(frame.opaque_runs, 1);
        assert_eq!((stats.set_clusters, stats.set_clusters_culled), (6, 4));
        assert_eq!(stats.set_instances, 24);
        // Every cluster casts, whether it is in view or not.
        assert_eq!(frame.casters.len(), 10);

        // A draw distance of 20 m leaves clusters 4 (0.1 m away: the camera
        // stands in it) and 5 (9.1 m), then a minimum of 5 m only cluster 5.
        for ((min, max), runs) in [((0.0, 20.0), (16, 8)), ((5.0, 20.0), (20, 4))] {
            let mut frame = SetFrame::default();
            let mut stats = FrameStats::default();
            frame.cull(
                &clusters,
                &boxes,
                template(),
                Distances::new(min, max),
                false,
                false,
                &frustum,
                eye,
                &mut stats,
            );
            assert_eq!(
                frame.runs,
                [Run {
                    first: runs.0,
                    count: runs.1,
                    ..template()
                }],
                "{min}..{max}"
            );
            assert!(frame.casters.is_empty());
        }
    }

    #[test]
    fn gaps_split_runs_and_blended_clusters_are_drawn_far_to_near() {
        // The camera at x = 70 looking back along -x sees clusters 0 to 4;
        // a distance range of 5 to 50 m drops cluster 4 (0.1 m away) and
        // cluster 0 (57 m).
        let (clusters, boxes) = row_boxes(40, 4);
        let (frustum, eye) = view(
            Vec3::new(70.0, 1.0, 0.0),
            Vec3::new(-100.0, 1.0, 0.0),
            500.0,
        );
        let mut frame = SetFrame::default();
        let mut stats = FrameStats::default();
        frame.cull(
            &clusters,
            &boxes,
            template(),
            Distances::new(5.0, 50.0),
            true,
            false,
            &frustum,
            eye,
            &mut stats,
        );
        frame.finish();
        assert_eq!(frame.opaque_runs, 0);
        let firsts: Vec<u32> = frame.runs.iter().map(|r| r.first).collect();
        assert_eq!(
            firsts,
            [4, 8, 12],
            "one run per cluster, the farthest first"
        );

        // Two parts of one set: the opaque runs sort by pipeline, material
        // and mesh, keeping the snapshot's order among equals.
        let mut frame = SetFrame::default();
        let everywhere = Distances::new(0.0, 0.0);
        for (variant, mesh) in [(1, (9, 0)), (0, (8, 0)), (0, (7, 0))] {
            let run = Run {
                variant,
                mesh,
                entry: frame.entries.len() as u32,
                ..template()
            };
            frame.cull(
                &clusters, &boxes, run, everywhere, false, false, &frustum, eye, &mut stats,
            );
            frame.entries.push(SetPartUniform::new(
                &Mat4::IDENTITY,
                &Mat4::IDENTITY,
                DrawFlags::NONE,
            ));
        }
        frame.finish();
        let order: Vec<(usize, Slot, u32)> = frame
            .runs
            .iter()
            .map(|r| (r.variant, r.mesh, r.entry))
            .collect();
        assert_eq!(order, [(0, (7, 0), 2), (0, (8, 0), 1), (1, (9, 0), 0)]);
    }

    #[test]
    fn a_set_outside_the_view_or_its_range_tests_no_cluster() {
        let (clusters, boxes) = row_boxes(40, 4);
        // Looking away from the whole row.
        let (frustum, eye) = view(
            Vec3::new(-50.0, 1.0, 0.0),
            Vec3::new(-200.0, 1.0, 0.0),
            500.0,
        );
        let mut frame = SetFrame::default();
        let mut stats = FrameStats::default();
        let culled = frame.cull(
            &clusters,
            &boxes,
            template(),
            Distances::new(0.0, 0.0),
            false,
            false,
            &frustum,
            eye,
            &mut stats,
        );
        assert_eq!(culled, Culled::default());
        assert_eq!((stats.set_clusters, stats.set_clusters_culled), (0, 10));
        // Out of a casting part's range: no caster either.
        let (frustum, eye) = view(
            Vec3::new(-50.0, 1.0, 0.0),
            Vec3::new(200.0, 1.0, 0.0),
            500.0,
        );
        let culled = frame.cull(
            &clusters,
            &boxes,
            template(),
            Distances::new(0.0, 10.0),
            false,
            true,
            &frustum,
            eye,
            &mut stats,
        );
        assert_eq!(culled, Culled::default());
        assert!(frame.casters.is_empty() && frame.runs.is_empty());
    }

    #[test]
    fn each_cascade_takes_its_casters_in_runs_of_their_own() {
        let (clusters, boxes) = row_boxes(40, 4);
        let (frustum, eye) = view(Vec3::new(70.0, 1.0, 0.0), Vec3::new(200.0, 1.0, 0.0), 500.0);
        let mut frame = SetFrame::default();
        let mut stats = FrameStats::default();
        frame.cull(
            &clusters,
            &boxes,
            template(),
            Distances::new(0.0, 0.0),
            false,
            true,
            &frustum,
            eye,
            &mut stats,
        );
        // The sun straight above, so the depth along the light is -y.
        let light = frame::light_view(Vec3::NEG_Y);
        frame.light_boxes(&light);
        // A cascade square over x from 40 to 72, which clusters 2 to 4 reach.
        let middle = Aabb::new(Vec3::new(40.0, 0.0, -1.0), Vec3::new(72.0, 0.0, 1.0));
        let fit = CascadeBox {
            center: frame::transform_aabb(&middle, &light).center(),
            radius: 16.0,
            texel_world: 0.01,
        };
        let depth = frame.cascade(&fit);
        // And one holding every cluster.
        let fit_all = CascadeBox {
            radius: 1000.0,
            ..fit
        };
        frame.cascade(&fit_all);
        assert_eq!(frame.cascades, [0..1, 1..2]);
        let spans: Vec<(u32, u32, usize)> = frame
            .cascade_runs
            .iter()
            .map(|r| (r.first, r.count, r.variant))
            .collect();
        assert_eq!(spans, [(8, 12, 0), (0, 40, 0)]);
        // The nearest depth along the light: the top of the grown boxes.
        let top = boxes.clusters[2].max.y;
        assert!((depth.unwrap() - (-top)).abs() < 1e-4, "{depth:?} {top}");
    }
}
