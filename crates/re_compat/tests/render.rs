//! WP15: the 3D view as a host sees it (parts as draw items, merged static
//! cells, materials, lighting, the camera's methods, particles, beams,
//! trails and highlights) and as golden images drawn through re_render.

use std::collections::HashMap;

use glam::{Vec2, Vec3};
use re_compat::SignalBehavior;
use re_compat::render::{self, MaterialKey, MeshKey, RenderFrame, RenderInput, RenderSettings};
use re_types::render::{
    DrawFlags, DrawItem, MaterialHandle, MeshHandle, RenderSnapshot, TextureHandle, TextureRef,
};
use wp15::{Place, place_in};

mod wp15;

const SERVICES: &[&str] = &["Workspace", "ReplicatedStorage", "Lighting"];

fn place() -> Place {
    place_in(SignalBehavior::Deferred, SERVICES)
}

fn input() -> RenderInput {
    RenderInput {
        viewport: Vec2::new(800.0, 600.0),
        top_inset: 36.0,
        dt: 1.0 / 60.0,
    }
}

const NEW: &str = r#"
local function new(class, props, parent)
    local i = Instance.new(class)
    for k, v in pairs(props) do
        i[k] = v
    end
    i.Parent = parent
    return i
end
"#;

fn script(p: &mut Place, src: &str) {
    p.run(&format!("{NEW}\n{src}"));
}

fn eval(p: &mut Place, src: &str) -> Vec<String> {
    p.out();
    script(p, src);
    p.out()
}

#[test]
fn time_of_day_moves_the_sun() {
    let mut p = place();
    let out = eval(
        &mut p,
        r#"
        local l = game:GetService("Lighting")
        l.ClockTime = 6
        local d = l:GetSunDirection()
        print(string.format("%.2f", d.X), math.abs(d.Y) < 1e-3, l.TimeOfDay)
        l.TimeOfDay = "18:30:00"
        print(l.ClockTime, l:GetMinutesAfterMidnight())
        l:SetMinutesAfterMidnight(12 * 60)
        local m = l:GetMoonDirection()
        print(l.TimeOfDay, string.format("%.2f", m.Y))
        print(pcall(function() l.TimeOfDay = "noon" end))
        "#,
    );
    assert_eq!(out[0], "1.00\ttrue\t06:00:00");
    assert_eq!(out[1], "18.5\t1110");
    assert_eq!(out[2], "12:00:00\t-0.75");
    assert!(out[3].starts_with("false"), "{out:?}");

    // Noon at the default latitude: the sun is high, the sky is day.
    let f = p.render(&input());
    let sun = f
        .lights
        .iter()
        .find(|l| l.kind == re_types::render::LightKind::Directional);
    assert!(sun.is_some_and(|s| s.direction.y < -0.7), "{:?}", f.lights);
    assert!(f.sky.zenith[2] > f.sky.zenith[0]);
    assert!(f.fog.is_none());
}

#[test]
fn parts_become_items_with_shared_meshes_and_materials() {
    let mut p = place();
    script(
        &mut p,
        r#"
        for i = 1, 3 do
            new("Part", {Size = Vector3.new(2, 2, 2), Position = Vector3.new(i * 4, 5, 0),
                Color = Color3.fromRGB(200, 40, 40), Material = Enum.Material.Wood}, workspace)
        end
        new("Part", {Size = Vector3.new(2, 2, 2), Position = Vector3.new(0, 5, 0), Transparency = 1}, workspace)
        new("Part", {Size = Vector3.new(2, 2, 2), Position = Vector3.new(0, 9, 0), LocalTransparencyModifier = 0.5}, workspace)
        new("Part", {Shape = Enum.PartType.Ball, Size = Vector3.new(3, 3, 3), Position = Vector3.new(0, 13, 0),
            Material = Enum.Material.Neon, CastShadow = false}, workspace)
        new("Part", {Size = Vector3.new(2, 2, 2), Position = Vector3.new(0, 20, 0)}, game:GetService("ReplicatedStorage"))
        "#,
    );
    let f = p.render(&input());
    assert_eq!(f.items.len(), 5, "{:#?}", f.items);
    // The three wooden blocks and the faded one share a mesh; the ball has its own.
    assert_eq!(f.new_meshes.len(), 2);
    let wood = MaterialKey {
        material: render::MATERIALS.iter().position(|m| *m == "Wood").unwrap() as u8,
        color: [200, 40, 40],
        alpha: 255,
        reflectance: 0,
    };
    assert_eq!(f.items.iter().filter(|i| i.material == wood).count(), 3);
    assert!(f.items.iter().any(|i| i.material.alpha == 128));
    let neon = f.items.iter().find(|i| i.material.material == 2).unwrap();
    assert!(!neon.cast_shadow);
    // Wood's and Plastic's textures come with their materials, once each;
    // Neon is smooth.
    let mut names: Vec<&str> = f.new_textures.iter().map(|t| t.0.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["compat:material/Plastic", "compat:material/Wood"]);
    let (_, desc) = f.new_materials.iter().find(|(k, _)| *k == wood).unwrap();
    assert_eq!(
        desc.base_color_texture,
        Some(TextureRef::Asset("compat:material/Wood".into()))
    );

    // The next frame sends nothing new.
    let g = p.render(&input());
    assert_eq!(g.items.len(), 5);
    assert!(g.new_meshes.is_empty() && g.new_materials.is_empty() && g.new_textures.is_empty());
}

#[test]
fn viewport_frames_show_their_parts_through_their_camera() {
    let mut p = place();
    script(
        &mut p,
        r#"
        local holder = new("Folder", {Name = "Holder"}, game:GetService("ReplicatedStorage"))
        local vf = new("ViewportFrame", {Ambient = Color3.new(0, 0, 0),
            LightColor = Color3.new(1, 1, 1), LightDirection = Vector3.new(0, -2, 0)}, holder)
        local model = new("WorldModel", {}, vf)
        new("Part", {Size = Vector3.new(2, 2, 2), Color = Color3.fromRGB(10, 200, 30)}, model)
        new("Part", {Shape = Enum.PartType.Ball, Size = Vector3.new(3, 3, 3), Position = Vector3.new(4, 0, 0)}, vf)
        new("Part", {Size = Vector3.new(2, 2, 2), Transparency = 1}, vf)
        S.vf, S.model = vf, model
        new("Part", {Size = Vector3.new(2, 2, 2), Position = Vector3.new(0, 5, 0)}, workspace)
        "#,
    );
    // Without a camera it shows nothing.
    let f = p.render(&input());
    assert_eq!(f.items.len(), 1);
    assert_eq!(f.viewports.len(), 1);
    assert!(f.viewports[0].camera.is_none() && f.viewports[0].items.is_empty());
    script(
        &mut p,
        r#"
        local cam = new("Camera", {CFrame = CFrame.lookAt(Vector3.new(0, 0, 10), Vector3.zero),
            FieldOfView = 40}, S.vf)
        S.vf.CurrentCamera = cam
        "#,
    );
    let f = p.render(&input());
    // Its parts are not in the 3D view; the transparent one is not drawn.
    assert_eq!(f.items.len(), 1);
    let v = &f.viewports[0];
    let cam = v.camera.unwrap();
    assert!((cam.fov_y_radians - 40f32.to_radians()).abs() < 1e-5);
    assert_eq!(cam.world.w_axis.truncate(), Vec3::new(0.0, 0.0, 10.0));
    assert_eq!(v.items.len(), 2);
    assert!(v.items.iter().any(|i| i.material.color == [10, 200, 30]));
    assert_eq!(v.ambient, [0.0; 3]);
    assert_eq!(v.light.color, [1.0; 3]);
    assert_eq!(v.light.direction, Vec3::NEG_Y);
    assert!(!v.light.shadows);
    // Their meshes and materials come with the frame like the view's.
    for i in &v.items {
        let sent = |k| f.new_meshes.iter().any(|(m, _)| *m == k);
        assert!(sent(i.mesh) || f.items.iter().any(|j| j.mesh == i.mesh));
    }
    // A WorldModel holds parts but answers no spatial queries.
    let out = eval(
        &mut p,
        "print(pcall(function() return S.model:Raycast(Vector3.zero, Vector3.yAxis) end))",
    );
    assert!(
        out[0].starts_with("false") && out[0].contains("RE9001"),
        "{out:?}"
    );
}

#[test]
fn static_cells_merge_into_one_mesh_per_material() {
    let mut p = place();
    script(
        &mut p,
        r#"
        for x = 0, 3 do
            for z = 0, 3 do
                new("Part", {Anchored = true, Size = Vector3.new(4, 1, 4), Position = Vector3.new(x * 4, 0.5, z * 4),
                    Color = Color3.fromHSV((x + z) / 8, 0.6, 0.9),
                    Material = (x + z) % 2 == 0 and Enum.Material.Plastic or Enum.Material.Slate}, workspace)
            end
        end
        S.glass = new("Part", {Anchored = true, Size = Vector3.new(4, 1, 4), Position = Vector3.new(0, 1, 0),
            Transparency = 0.5}, workspace)
        S.loose = new("Part", {Size = Vector3.new(1, 1, 1), Position = Vector3.new(0, 10, 0)}, workspace)
        "#,
    );
    p.physics(1.0 / 60.0);
    let f = p.render(&input());
    let merged: Vec<_> = f
        .items
        .iter()
        .filter(|i| i.world == glam::Mat4::IDENTITY)
        .collect();
    // Plastic and Slate, each one mesh in white with the colours in it.
    assert_eq!(merged.len(), 2, "{:#?}", f.items);
    assert!(merged.iter().all(|i| i.material.color == [255; 3]));
    // Plus the glass part and the loose part on their own.
    assert_eq!(f.items.len(), 4);
    let mesh = f
        .new_meshes
        .iter()
        .find(|(k, _)| *k == merged[0].mesh)
        .map(|(_, m)| m)
        .unwrap();
    assert_eq!(mesh.vertices.len(), 8 * 24);
    let colors: std::collections::HashSet<[u32; 3]> = mesh
        .vertices
        .iter()
        .map(|v| {
            [
                v.color[0].to_bits(),
                v.color[1].to_bits(),
                v.color[2].to_bits(),
            ]
        })
        .collect();
    assert!(colors.len() > 1);

    // Unchanged, the cell is not rebuilt; recoloured, it is.
    p.physics(1.0 / 60.0);
    let g = p.render(&input());
    assert!(g.new_meshes.is_empty() && g.released_meshes.is_empty());
    script(
        &mut p,
        "workspace:GetChildren()[3].Color = Color3.new(0, 0, 0)",
    );
    p.physics(1.0 / 60.0);
    let h = p.render(&input());
    assert_eq!(h.new_meshes.len(), 2);
    assert_eq!(h.released_meshes.len(), 2);

    // A highlighted part leaves the merge to be drawn on its own.
    script(
        &mut p,
        r#"new("Highlight", {}, workspace:GetChildren()[3])"#,
    );
    let k = p.render(&input());
    assert_eq!(k.highlights.len(), 1);
    assert_eq!(k.items.len(), 5);
    let id = k.highlights[0].entities[0];
    assert!(
        k.items
            .iter()
            .any(|i| i.entity == id && i.world != glam::Mat4::IDENTITY),
        "{id:?} in {:#?}",
        k.items
    );
}

#[test]
fn camera_methods_project_and_cast_rays() {
    let mut p = place();
    let f = p.render(&input());
    assert!((f.camera.fov_y_radians - 70f32.to_radians()).abs() < 1e-5);
    let out = eval(
        &mut p,
        r#"
        local cam = workspace.CurrentCamera
        cam.CFrame = CFrame.new(Vector3.new(0, 10, 10), Vector3.new(0, 10, 0))
        print(cam.ViewportSize)
        local v, on = cam:WorldToViewportPoint(Vector3.new(0, 10, 0))
        print(string.format("%g %g %g", v.X, v.Y, v.Z), on)
        local s = cam:WorldToScreenPoint(Vector3.new(0, 10, 0))
        print(string.format("%g %g", s.X, s.Y))
        local _, behind = cam:WorldToViewportPoint(Vector3.new(0, 10, 20))
        print(behind)
        local r = cam:ViewportPointToRay(400, 300, 2)
        local function v3(v) return string.format("%g,%g,%g", v.X, v.Y, v.Z) end
        print(typeof(r), v3(r.Origin), v3(r.Direction))
        local r2 = cam:ScreenPointToRay(400, 264)
        print(string.format("%.3f", r2.Direction.Y))
        local ray = Ray.new(Vector3.zero, Vector3.new(10, 0, 0))
        print(v3(ray:ClosestPoint(Vector3.new(3, 5, 0))), ray:Distance(Vector3.new(3, 4, 0)), v3(ray.Unit.Direction))
        "#,
    );
    assert_eq!(out[0], "800, 600");
    assert_eq!(out[1], "400 300 10\ttrue");
    assert_eq!(out[2], "400 264");
    assert_eq!(out[3], "false");
    assert_eq!(out[4], "Ray\t0,10,8\t0,0,-1");
    assert_eq!(out[5], "0.000");
    assert_eq!(out[6], "3,0,0\t4\t1,0,0");
    let f = p.render(&input());
    assert_eq!(f.camera.position(), Vec3::new(0.0, 10.0, 10.0));
}

#[test]
fn lights_spots_fog_and_color_correction() {
    let mut p = place();
    script(
        &mut p,
        r#"
        local base = new("Part", {Anchored = true, Size = Vector3.new(2, 2, 2), Position = Vector3.new(0, 4, 0)}, workspace)
        new("PointLight", {Range = 12, Brightness = 2, Color = Color3.new(1, 0.5, 0)}, base)
        new("SpotLight", {Range = 20, Angle = 60, Face = Enum.NormalId.Bottom}, base)
        new("PointLight", {Enabled = false}, base)
        local l = game:GetService("Lighting")
        l.FogEnd = 200
        l.FogStart = 20
        new("ColorCorrectionEffect", {Saturation = -1, Brightness = 0.1}, l)
        new("Atmosphere", {}, l)
        l.ExposureCompensation = 1
        "#,
    );
    let f = p.render(&input());
    let points: Vec<_> = f
        .lights
        .iter()
        .filter(|l| l.kind == re_types::render::LightKind::Point)
        .collect();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].range, 12.0);
    assert_eq!(points[0].position, Vec3::new(0.0, 4.0, 0.0));
    assert_eq!(f.spot_lights.len(), 1);
    assert!((f.spot_lights[0].direction - Vec3::NEG_Y).length() < 1e-5);
    assert!((f.spot_lights[0].half_angle - 30f32.to_radians()).abs() < 1e-5);
    let fog = f.fog.unwrap();
    assert_eq!((fog.start, fog.end), (20.0, 200.0));
    let g = f.grading.unwrap();
    assert_eq!((g.saturation, g.brightness), (-1.0, 0.1));
    assert_eq!(f.exposure, 2.0);
}

#[test]
fn particles_beams_and_trails_follow_their_properties_and_the_budget() {
    let mut p = place();
    let lua_seed = RenderSettings {
        effect_budget: 50,
        seed: 7,
    };
    p.lua(|lua| render::configure(lua, lua_seed));
    script(
        &mut p,
        r#"
        local a = new("Part", {Anchored = true, Size = Vector3.new(1, 1, 1), Position = Vector3.new(0, 5, 0)}, workspace)
        local b = new("Part", {Anchored = true, Size = Vector3.new(1, 1, 1), Position = Vector3.new(10, 5, 0)}, workspace)
        local a0 = new("Attachment", {}, a)
        local a1 = new("Attachment", {}, b)
        new("Beam", {Attachment0 = a0, Attachment1 = a1, CurveSize0 = 3, Segments = 8}, workspace)
        S.mover = new("Part", {Anchored = true, Size = Vector3.new(1, 1, 1), Position = Vector3.new(0, 10, 0)}, workspace)
        local t0 = new("Attachment", {Position = Vector3.new(0, 0.5, 0)}, S.mover)
        local t1 = new("Attachment", {Position = Vector3.new(0, -0.5, 0)}, S.mover)
        S.trail = new("Trail", {Attachment0 = t0, Attachment1 = t1, Lifetime = 1}, S.mover)
        S.e = new("ParticleEmitter", {Rate = 0, Lifetime = NumberRange.new(2), Speed = NumberRange.new(4)}, a)
        S.e:Emit(10)
        "#,
    );
    let f = p.render(&input());
    assert_eq!(f.ribbons.len(), 1, "the trail has one point so far");
    assert_eq!(f.ribbons[0].points.len(), 9);
    assert_eq!(f.sprites.len(), 10);
    assert_eq!(f.effects.drawn, 8 + 10);
    // The particles leave upwards (EmissionDirection Top) at Speed 4.
    for _ in 0..5 {
        script(&mut p, "S.mover.Position += Vector3.new(1, 0, 0)");
        p.render(&input());
    }
    let f = p.render(&input());
    assert!(f.sprites.iter().all(|s| s.position.y > 5.0));
    let trail = &f.ribbons[1];
    assert_eq!(trail.points.len(), 6);
    assert!((trail.points.last().unwrap().width - 1.0).abs() < 1e-5);

    // Over the budget: the overflow is dropped and warned about once.
    script(&mut p, "S.e:Emit(100)");
    let f = p.render(&input());
    assert_eq!(f.effects.drawn, 50);
    assert!(f.effects.dropped > 0);
    assert_eq!(f.diagnostics.len(), 1);
    assert_eq!(f.diagnostics[0].code, re_compat::codes::EFFECT_BUDGET);
    let g = p.render(&input());
    assert!(g.effects.dropped > 0 && g.diagnostics.is_empty());
    script(&mut p, "S.e:Clear() S.trail:Clear()");
    let h = p.render(&input());
    assert!(h.sprites.is_empty() && h.effects.dropped == 0);
}

#[test]
fn same_seed_same_particles() {
    let run = || {
        let mut p = place();
        p.lua(|lua| {
            render::configure(
                lua,
                RenderSettings {
                    effect_budget: 1000,
                    seed: 3,
                },
            )
        });
        script(
            &mut p,
            r#"
            local a = new("Part", {Anchored = true, Size = Vector3.new(4, 1, 4)}, workspace)
            new("ParticleEmitter", {Rate = 30, SpreadAngle = Vector2.new(30, 30)}, a)
            "#,
        );
        let mut last = Vec::new();
        for _ in 0..30 {
            last = p.render(&input()).sprites;
        }
        last
    };
    let (a, b) = (run(), run());
    assert!(!a.is_empty());
    assert_eq!(a, b);
}

/// A renderer that draws compat frames: the host side of [`RenderFrame`].
struct View3d {
    gpu: re_render::Gpu,
    renderer: re_render::Renderer,
    capture: re_render::Capture,
    meshes: HashMap<MeshKey, MeshHandle>,
    materials: HashMap<MaterialKey, MaterialHandle>,
    textures: HashMap<String, TextureHandle>,
    set: &'static str,
    size: (u32, u32),
}

impl View3d {
    fn new(w: u32, h: u32) -> Option<View3d> {
        let backends = re_render::backends_from_env().expect("RE_BACKENDS");
        let gpu = match re_render::Gpu::headless(backends) {
            Ok(g) => g,
            Err(d) if std::env::var_os("CI").is_none() => {
                println!("skipped: no adapter: {}", d.render_human());
                return None;
            }
            Err(d) => panic!("{}", d.render_human()),
        };
        let set = match gpu.backend() {
            re_render::wgpu::Backend::Vulkan => "vulkan",
            re_render::wgpu::Backend::Gl => "gl",
            other => {
                println!("no golden set for {other:?}");
                return None;
            }
        };
        let renderer = re_render::Renderer::new(&gpu, re_render::Capture::FORMAT);
        let capture = re_render::Capture::new(&gpu.device, w, h).unwrap();
        Some(View3d {
            gpu,
            renderer,
            capture,
            meshes: HashMap::new(),
            materials: HashMap::new(),
            textures: HashMap::new(),
            set,
            size: (w, h),
        })
    }

    /// Uploads and releases what `f` says, then draws it.
    fn draw(&mut self, f: &RenderFrame) -> re_render::FrameReport {
        let r = &mut self.renderer;
        for (name, t) in &f.new_textures {
            self.textures
                .insert(name.clone(), r.upload_texture(t).unwrap());
        }
        for (k, m) in &f.new_meshes {
            self.meshes.insert(*k, r.upload_mesh(m).unwrap());
        }
        for (k, d) in &f.new_materials {
            let mut d = d.clone();
            if let Some(TextureRef::Asset(name)) = &d.base_color_texture {
                d.base_color_texture = self.textures.get(name).map(|h| TextureRef::Handle(*h));
            }
            self.materials.insert(*k, r.create_material(&d).unwrap());
        }
        for k in &f.released_meshes {
            if let Some(h) = self.meshes.remove(k) {
                r.release_mesh(h).unwrap();
            }
        }
        for k in &f.released_materials {
            if let Some(h) = self.materials.remove(k) {
                r.release_material(h).unwrap();
            }
        }
        let mut snapshot = RenderSnapshot {
            camera: f.camera,
            lights: f.lights.clone(),
            items: f
                .items
                .iter()
                .map(|i| DrawItem {
                    entity: i.entity,
                    mesh: self.meshes[&i.mesh],
                    material: self.materials[&i.material],
                    world: i.world,
                    flags: if i.cast_shadow {
                        DrawFlags::CAST_SHADOWS | DrawFlags::RECEIVE_SHADOWS
                    } else {
                        DrawFlags::RECEIVE_SHADOWS
                    },
                })
                .collect(),
            clear: f.clear,
            ..RenderSnapshot::default()
        };
        snapshot.post.exposure = f.exposure;
        snapshot.post.grading = f.grading.map(|g| re_types::render::ColorGrading {
            brightness: g.brightness,
            contrast: g.contrast,
            saturation: g.saturation,
            tint: g.tint,
        });
        let extras = extras(f);
        r.render_with(
            &self.gpu.device,
            &self.gpu.queue,
            self.capture.view(),
            &snapshot,
            &extras,
        )
        .unwrap_or_else(|d| panic!("{}", d.render_human()))
    }

    fn image(&self) -> re_render::Image {
        assert_eq!(
            self.gpu.errors().count(),
            0,
            "{:?}",
            self.gpu.errors().take()
        );
        self.capture
            .read(&self.gpu.device, &self.gpu.queue)
            .unwrap()
    }

    /// Compares the target with `tests/fixtures/render/goldens/<set>/<name>.png`;
    /// `RE_UPDATE_GOLDENS=1` rewrites it.
    fn golden(&self, name: &str) {
        use re_render::imgdiff::{self, Tolerance};
        let image = self.image();
        assert!(image.stats([0; 4]).distinct_colors > 50);
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let out = root.join("../../out/re_compat").join(self.set);
        image.save_png(&out.join(format!("{name}.png"))).unwrap();
        let golden = root
            .join("tests/fixtures/render/goldens")
            .join(self.set)
            .join(format!("{name}.png"));
        if std::env::var_os("RE_UPDATE_GOLDENS").is_some_and(|v| !v.is_empty()) {
            image.save_png(&golden).unwrap();
            println!("updated {}", golden.display());
            return;
        }
        let expected = re_render::Image::load_png(&golden).unwrap_or_else(|d| {
            panic!(
                "{}missing golden: run with RE_UPDATE_GOLDENS=1 and review the image",
                d.render_human()
            )
        });
        let report = imgdiff::compare(&image, &expected, Tolerance::default()).unwrap();
        println!("{name} ({}): {}", self.set, report.summary());
        if !report.matches() {
            let diff = imgdiff::diff_image(&image, &expected, Tolerance::default()).unwrap();
            diff.save_png(&out.join(format!("{name}.diff.png")))
                .unwrap();
            panic!(
                "{name} does not match its {} golden: {} (actual and diff in {})",
                self.set,
                report.summary(),
                out.display()
            );
        }
    }

    fn input(&self) -> RenderInput {
        RenderInput {
            viewport: Vec2::new(self.size.0 as f32, self.size.1 as f32),
            top_inset: 0.0,
            dt: 1.0 / 30.0,
        }
    }
}

/// The renderer's extras of a frame.
fn extras(f: &RenderFrame) -> re_render::SceneExtras {
    use re_render as rr;
    rr::SceneExtras {
        ambient: Some(rr::Ambient {
            sky: f.ambient.0,
            ground: f.ambient.1,
        }),
        fog: f.fog.map(|g| rr::Fog {
            start: g.start,
            end: g.end,
            color: g.color,
        }),
        sky: Some(rr::Sky {
            zenith: f.sky.zenith,
            horizon: f.sky.horizon,
            ground: f.sky.ground,
            sun: f.sky.sun,
        }),
        spot_lights: f
            .spot_lights
            .iter()
            .map(|s| rr::SpotLight {
                position: s.position,
                direction: s.direction,
                color: s.color,
                intensity: s.intensity,
                range: s.range,
                half_angle: s.half_angle,
            })
            .collect(),
        sprites: f
            .sprites
            .iter()
            .map(|s| rr::Sprite {
                position: s.position,
                size: s.size,
                rotation: s.rotation,
                color: s.color,
                emission: s.emission,
                shape: match s.shape {
                    render::SpriteShape::Soft => rr::SpriteShape::Soft,
                    render::SpriteShape::Square => rr::SpriteShape::Square,
                },
            })
            .collect(),
        ribbons: f
            .ribbons
            .iter()
            .map(|r| rr::Ribbon {
                points: r
                    .points
                    .iter()
                    .map(|p| rr::RibbonPoint {
                        position: p.position,
                        width: p.width,
                        color: p.color,
                    })
                    .collect(),
                normal: r.normal,
                emission: r.emission,
            })
            .collect(),
        highlights: f
            .highlights
            .iter()
            .map(|h| rr::Highlight {
                entities: h.entities.clone(),
                fill: h.fill,
                outline: h.outline,
                always_on_top: h.always_on_top,
            })
            .collect(),
    }
}

/// Every material on a block, plus the four shapes, a reflective block, a
/// transparent one and a highlighted one, under the afternoon sun.
#[test]
fn golden_material_sheet() {
    let Some(mut v) = View3d::new(640, 400) else {
        return;
    };
    let mut p = place();
    script(
        &mut p,
        r#"
        local cam = workspace.CurrentCamera
        cam.CFrame = CFrame.new(Vector3.new(0, 26, 30), Vector3.new(0, 0, 2))
        cam.FieldOfView = 50
        new("Part", {Anchored = true, Size = Vector3.new(80, 1, 60), Position = Vector3.new(0, -1, 0),
            Color = Color3.fromRGB(90, 90, 95), Material = Enum.Material.Concrete}, workspace)
        local i = 0
        for _, m in ipairs(Enum.Material:GetEnumItems()) do
            local x, z = i % 9, i // 9
            new("Part", {Anchored = true, Size = Vector3.new(3, 1, 3),
                Position = Vector3.new((x - 4) * 4, 0, (z - 2) * 4),
                Color = Color3.fromHSV((i % 9) / 9, 0.35, 0.85), Material = m}, workspace)
            i += 1
        end
        local shapes = {Enum.PartType.Ball, Enum.PartType.Cylinder, Enum.PartType.Wedge, Enum.PartType.CornerWedge}
        for k, s in ipairs(shapes) do
            new("Part", {Anchored = true, Shape = s, Size = Vector3.new(3, 3, 3),
                Position = Vector3.new((k - 3) * 5, 1.5, 12), Color = Color3.fromRGB(230, 230, 230)}, workspace)
        end
        new("Part", {Anchored = true, Size = Vector3.new(3, 3, 3), Position = Vector3.new(12, 1.5, 12),
            Reflectance = 0.8, Color = Color3.fromRGB(200, 200, 210)}, workspace)
        new("Part", {Anchored = true, Size = Vector3.new(3, 3, 3), Position = Vector3.new(-14, 1.5, 12),
            Transparency = 0.5, Color = Color3.fromRGB(60, 140, 255)}, workspace)
        local lit = new("Part", {Anchored = true, Size = Vector3.new(2, 2, 2), Position = Vector3.new(16, 1, 4),
            Color = Color3.fromRGB(255, 200, 80)}, workspace)
        new("Highlight", {FillColor = Color3.fromRGB(255, 80, 80), FillTransparency = 0.6}, lit)
        "#,
    );
    p.physics(1.0 / 60.0);
    let f = p.render(&v.input());
    assert!(
        f.items.len() < 60,
        "the static blocks are merged: {}",
        f.items.len()
    );
    let report = v.draw(&f);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    v.golden("material_sheet");
}

/// A curved beam, a trail swept over frames and a burst of particles at a
/// fixed seed, at dusk.
#[test]
fn golden_effects() {
    let Some(mut v) = View3d::new(480, 320) else {
        return;
    };
    let mut p = place();
    p.lua(|lua| {
        render::configure(
            lua,
            RenderSettings {
                effect_budget: 20_000,
                seed: 11,
            },
        )
    });
    script(
        &mut p,
        r#"
        game:GetService("Lighting").ClockTime = 18.2
        local cam = workspace.CurrentCamera
        cam.CFrame = CFrame.new(Vector3.new(0, 8, 22), Vector3.new(0, 5, 0))
        new("Part", {Anchored = true, Size = Vector3.new(60, 1, 40), Position = Vector3.new(0, -0.5, 0),
            Color = Color3.fromRGB(50, 55, 60)}, workspace)
        local a = new("Part", {Anchored = true, Size = Vector3.new(1, 1, 1), Position = Vector3.new(-9, 2, 0)}, workspace)
        local b = new("Part", {Anchored = true, Size = Vector3.new(1, 1, 1), Position = Vector3.new(-1, 2, 0)}, workspace)
        local a0 = new("Attachment", {Orientation = Vector3.new(0, 0, 90)}, a)
        local a1 = new("Attachment", {Orientation = Vector3.new(0, 0, 90)}, b)
        new("Beam", {Attachment0 = a0, Attachment1 = a1, CurveSize0 = 6, CurveSize1 = -6, Segments = 20,
            Width0 = 0.6, Width1 = 0.2, FaceCamera = true, LightEmission = 1,
            Color = ColorSequence.new(Color3.fromRGB(80, 200, 255), Color3.fromRGB(255, 80, 200)),
            Transparency = NumberSequence.new(0.1)}, workspace)
        S.mover = new("Part", {Anchored = true, Size = Vector3.new(0.5, 0.5, 0.5), Position = Vector3.new(2, 3, 0),
            Color = Color3.fromRGB(255, 220, 120), Material = Enum.Material.Neon}, workspace)
        local t0 = new("Attachment", {Position = Vector3.new(0, 0.6, 0)}, S.mover)
        local t1 = new("Attachment", {Position = Vector3.new(0, -0.6, 0)}, S.mover)
        new("Trail", {Attachment0 = t0, Attachment1 = t1, Lifetime = 1, FaceCamera = true, LightEmission = 0.5,
            Color = ColorSequence.new(Color3.fromRGB(255, 200, 80), Color3.fromRGB(255, 60, 30)),
            Transparency = NumberSequence.new(0, 1)}, S.mover)
        local src = new("Part", {Anchored = true, Size = Vector3.new(2, 0.2, 2), Position = Vector3.new(7, 0.1, 0),
            Color = Color3.fromRGB(40, 40, 40)}, workspace)
        S.e = new("ParticleEmitter", {Rate = 0, Lifetime = NumberRange.new(1.5, 2.5), Speed = NumberRange.new(4, 7),
            SpreadAngle = Vector2.new(25, 25), Acceleration = Vector3.new(0, -3, 0), LightEmission = 0.7,
            Size = NumberSequence.new(0.6, 0.1), Transparency = NumberSequence.new(0, 1),
            Color = ColorSequence.new(Color3.fromRGB(255, 240, 160), Color3.fromRGB(255, 90, 20))}, src)
        S.e:Emit(80)
        "#,
    );
    let mut f = p.render(&v.input());
    v.draw(&f);
    for i in 0..24 {
        let a = i as f32 * 0.26;
        script(
            &mut p,
            &format!(
                "S.mover.Position = Vector3.new({}, {}, 0)",
                2.0 + 3.0 * a.sin(),
                4.0 + 2.0 * (a * 0.5).cos()
            ),
        );
        f = p.render(&v.input());
        v.draw(&f);
    }
    assert!(f.sprites.len() > 40, "{}", f.sprites.len());
    assert_eq!(f.ribbons.len(), 2);
    v.draw(&f);
    v.golden("effects");
}

/// The same `BillboardGui` (sized in studs) over parts 10, 25 and 60 studs
/// away: it shrinks with distance and is drawn over the 3D view.
#[test]
fn golden_billboard_distances() {
    let Some(mut v) = View3d::new(480, 320) else {
        return;
    };
    let mut p = place();
    script(
        &mut p,
        r#"
        local cam = workspace.CurrentCamera
        cam.CFrame = CFrame.new(Vector3.new(0, 4, 0), Vector3.new(0, 4, -1))
        new("Part", {Anchored = true, Size = Vector3.new(200, 1, 200), Position = Vector3.new(0, -0.5, 0),
            Color = Color3.fromRGB(70, 120, 70), Material = Enum.Material.Grass}, workspace)
        for k, d in ipairs({10, 25, 60}) do
            local post = new("Part", {Anchored = true, Size = Vector3.new(1, 4, 1),
                Position = Vector3.new((k - 2) * d * 0.4, 2, -d), Color = Color3.fromRGB(150, 110, 70),
                Material = Enum.Material.Wood}, workspace)
            local bb = new("BillboardGui", {Size = UDim2.new(4, 0, 1.5, 0), StudsOffset = Vector3.new(0, 3, 0)}, post)
            new("TextLabel", {Size = UDim2.fromScale(1, 1), BackgroundColor3 = Color3.fromRGB(30, 30, 40),
                TextColor3 = Color3.new(1, 1, 1), TextScaled = true, Text = d .. " studs"}, bb)
        end
        "#,
    );
    let ws = re_compat::instance::tree::find_child(&p.world, p.game, "Workspace", false).unwrap();
    let ctx = egui::Context::default();
    re_compat::gui::install_fonts(&ctx);
    let (w, h) = v.size;
    let view = Vec2::new(w as f32, h as f32);
    for n in 0..3 {
        let f = p.render(&v.input());
        v.draw(&f);
        let frame = re_compat::gui::GuiFrame {
            viewport: view,
            top_inset: 0.0,
            roots: vec![ws],
            camera: Some(f.gui_camera),
            input: Default::default(),
            images: Default::default(),
        };
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(view.x, view.y),
            )),
            max_texture_side: Some(8192),
            time: Some(f64::from(n) / 60.0),
            ..Default::default()
        };
        raw.viewports
            .entry(raw.viewport_id)
            .or_default()
            .native_pixels_per_point = Some(1.0);
        let mut drawn = 0;
        let mut output = ctx.run_ui(raw, |ui| {
            let out = p.gui(ui.ctx(), &frame);
            drawn = out.drawn;
            re_compat::gui::paint(ui.ctx(), out.shapes);
        });
        assert_eq!(drawn, 3, "frame {n}");
        let ui = re_render::UiFrame::from_output(&ctx, &mut output, [w, h]);
        v.renderer
            .paint_ui(&v.gpu.device, &v.gpu.queue, v.capture.view(), &ui)
            .unwrap_or_else(|d| panic!("{}", d.render_human()));
    }
    v.golden("billboard_distances");
}

#[test]
fn emitter_members_drawn_later_are_stored() {
    let mut p = place();
    let out = eval(
        &mut p,
        r#"
        local e = Instance.new("ParticleEmitter")
        print(e.Squash.Keypoints[1].Value, e.FlipbookLayout, e.FlipbookMode, e.FlipbookFramerate.Min, e.WindAffectsDrag)
        e.Squash = NumberSequence.new(0.5)
        e.FlipbookLayout = Enum.ParticleFlipbookLayout.Grid4x4
        e.FlipbookFramerate = NumberRange.new(10, 20)
        e.FlipbookStartRandom = true
        print(e.Squash.Keypoints[2].Value, e.FlipbookLayout, e.FlipbookFramerate.Max, e.FlipbookStartRandom)
        "#,
    );
    assert_eq!(
        out,
        [
            "0\tEnum.ParticleFlipbookLayout.None\tEnum.ParticleFlipbookMode.Loop\t1\tfalse",
            "0.5\tEnum.ParticleFlipbookLayout.Grid4x4\t20\ttrue",
        ]
    );
}
