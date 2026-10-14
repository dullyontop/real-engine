//! Rendering helpers + assertions shared by the `headless` binary and the tests.

use glam::{Mat4, Quat, Vec3, Vec4Swizzles};

use crate::readback::{self, ImageStats};
use crate::scene::{self, OffscreenTarget, SceneRenderer};
use crate::{Result, SpikeError};

/// Result of one offscreen scene frame read back to the CPU.
pub struct SceneFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub depth: Vec<f32>,
}

/// Render the scene once at `cube_angle` (radians) and read back color (and depth
/// when `read_depth`, see [`depth_copies_supported`]).
pub fn render_scene_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &SceneRenderer,
    target: &OffscreenTarget,
    cube_angle: f32,
    read_depth: bool,
) -> Result<SceneFrame> {
    scene.update(queue, target.aspect(), cube_angle);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("scene-frame"),
    });
    scene.encode(&mut encoder, &target.color_view, &target.depth_view, None);
    let (color_buf, color_pitch) = readback::encode_texture_copy(
        device,
        &mut encoder,
        &target.color,
        wgpu::TextureAspect::All,
    )?;
    // Depth32Float can be copied out (DepthOnly aspect): useful for editor picking.
    // Needs DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES (absent on the GL backend).
    let depth_copy = if read_depth {
        Some(readback::encode_texture_copy(
            device,
            &mut encoder,
            &target.depth,
            wgpu::TextureAspect::DepthOnly,
        )?)
    } else {
        None
    };
    let index = queue.submit([encoder.finish()]);
    let rgba = readback::map_and_unpad(
        device,
        &color_buf,
        Some(index.clone()),
        color_pitch,
        target.width * 4,
        target.height,
    )?;
    let depth = match depth_copy {
        Some((depth_buf, depth_pitch)) => {
            let bytes = readback::map_and_unpad(
                device,
                &depth_buf,
                Some(index),
                depth_pitch,
                target.width * 4,
                target.height,
            )?;
            bytemuck::cast_slice::<u8, f32>(&bytes).to_vec()
        }
        None => Vec::new(),
    };
    Ok(SceneFrame {
        width: target.width,
        height: target.height,
        rgba,
        depth,
    })
}

/// Whether depth textures can be copied to buffers on this adapter
/// (`DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES`; absent on the GL backend).
pub fn depth_copies_supported(adapter: &wgpu::Adapter) -> bool {
    adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES)
}

/// sRGB-encoded bytes of the scene clear color, as stored in an *Srgb target.
pub fn clear_color_srgb8() -> [u8; 4] {
    let c = scene::CLEAR_COLOR;
    [
        readback::linear_to_srgb_u8(c.r),
        readback::linear_to_srgb_u8(c.g),
        readback::linear_to_srgb_u8(c.b),
        255,
    ]
}

/// Project a world-space point to pixel coordinates (origin top-left).
pub fn project_to_pixel(view_proj: Mat4, p: Vec3, width: u32, height: u32) -> Option<(u32, u32)> {
    let clip = view_proj * p.extend(1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.xyz() / clip.w;
    let x = (ndc.x * 0.5 + 0.5) * width as f32;
    let y = (1.0 - (ndc.y * 0.5 + 0.5)) * height as f32;
    if x < 0.0 || y < 0.0 || x >= width as f32 || y >= height as f32 {
        return None;
    }
    Some((x as u32, y as u32))
}

#[derive(Debug, Clone)]
pub struct FaceSample {
    pub normal_world: Vec3,
    pub pixel: (u32, u32),
    pub rgba: [u8; 4],
    pub luma: f32,
}

#[derive(Debug, Clone)]
pub struct SceneCheck {
    pub stats: ImageStats,
    pub clear_srgb8: [u8; 4],
    pub faces: Vec<FaceSample>,
    pub depth_center: f32,
    pub depth_corner: f32,
    pub failures: Vec<String>,
}

/// Assert that the frame is a plausible, lit, depth-tested render:
/// center differs from the clear color and is cube-colored (orange: the plane is
/// drawn after the cube, so a broken depth test would show the plane there),
/// visible cube faces have distinct shading, the image has many colors and
/// luminance variation, and the depth buffer holds geometry depth at the
/// center and the clear value (1.0) in the sky corner.
pub fn check_scene_frame(frame: &SceneFrame, scene: &SceneRenderer, cube_angle: f32) -> SceneCheck {
    let clear = clear_color_srgb8();
    let stats = readback::image_stats(frame.width, frame.height, &frame.rgba, clear);
    let mut failures = Vec::new();

    if stats.center == clear {
        failures.push(format!(
            "center pixel {:?} equals the clear color",
            stats.center
        ));
    }
    let [r, g, b, _] = stats.center;
    if !(r > g && r > b) {
        failures.push(format!(
            "center pixel {:?} is not cube-orange (depth test or draw order broken?)",
            stats.center
        ));
    }
    let total = (frame.width * frame.height) as usize;
    if stats.pixels_differing_from_clear < total / 10 {
        failures.push(format!(
            "only {} of {} pixels differ from the clear color",
            stats.pixels_differing_from_clear, total
        ));
    }
    if stats.distinct_colors < 64 {
        failures.push(format!("only {} distinct colors", stats.distinct_colors));
    }
    if stats.luma_stddev_non_clear < 8.0 {
        failures.push(format!(
            "luma stddev {} too small (no shading?)",
            stats.luma_stddev_non_clear
        ));
    }

    // Per-face shading: sample the projected center of each camera-facing cube face.
    let aspect = frame.width as f32 / frame.height as f32;
    let view_proj = scene.camera.view_proj(aspect);
    let rot = Quat::from_rotation_y(cube_angle);
    let center = Vec3::new(0.0, 0.5, 0.0);
    let mut faces = Vec::new();
    for n in [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ] {
        let nw = rot * n;
        let face_center = center + nw * 0.5;
        if nw.dot(scene.camera.eye - face_center) <= 0.0 {
            continue; // back-facing
        }
        if let Some((x, y)) = project_to_pixel(view_proj, face_center, frame.width, frame.height) {
            let i = ((y * frame.width + x) * 4) as usize;
            let px = [
                frame.rgba[i],
                frame.rgba[i + 1],
                frame.rgba[i + 2],
                frame.rgba[i + 3],
            ];
            faces.push(FaceSample {
                normal_world: nw,
                pixel: (x, y),
                rgba: px,
                luma: readback::luma(&px),
            });
        }
    }
    if faces.len() < 2 {
        failures.push(format!(
            "expected >= 2 visible cube faces, got {}",
            faces.len()
        ));
    }
    for i in 0..faces.len() {
        for j in (i + 1)..faces.len() {
            if (faces[i].luma - faces[j].luma).abs() < 4.0 {
                failures.push(format!(
                    "faces {:?} and {:?} have nearly equal luma {:.1} / {:.1}",
                    faces[i].normal_world, faces[j].normal_world, faces[i].luma, faces[j].luma
                ));
            }
        }
    }

    let w = frame.width as usize;
    let (depth_center, depth_corner) = if frame.depth.is_empty() {
        (f32::NAN, f32::NAN) // depth readback unsupported on this adapter
    } else {
        let c = frame.depth[(frame.height as usize / 2) * w + w / 2];
        let k = frame.depth[0];
        if !(c > 0.0 && c < 1.0) {
            failures.push(format!("depth at center {c} not in (0,1)"));
        }
        if k != 1.0 {
            failures.push(format!("depth at top-left (sky) {k} != 1.0 (clear value)"));
        }
        (c, k)
    };

    SceneCheck {
        stats,
        clear_srgb8: clear,
        faces,
        depth_center,
        depth_corner,
        failures,
    }
}

/// Convenience: fail with all collected messages.
pub fn ensure_ok(check: &SceneCheck) -> Result<()> {
    if check.failures.is_empty() {
        Ok(())
    } else {
        Err(SpikeError::Other(check.failures.join("; ")))
    }
}
