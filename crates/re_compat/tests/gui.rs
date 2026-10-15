//! WP15: the 2D UI as scripts see it: layout values, text, input events and
//! billboards, in a client VM. The expected positions and sizes are worked
//! out by hand from the properties each test sets.

use glam::{Quat, Vec2, Vec3};
use re_compat::gui::{self, GuiCamera, GuiFrame, GuiInput, GuiOutput};
use re_compat::instance::tree;
use re_scene::Entity;
use wp15::{Place, place};

mod wp15;

const VIEW: Vec2 = Vec2::new(800.0, 600.0);
const INSET: f32 = 36.0;

/// A place with a `PlayerGui` folder (as `S.pg`) and an egui context with
/// the bundled fonts in.
struct Ui {
    place: Place,
    ctx: egui::Context,
    view: Vec2,
    pub camera: Option<GuiCamera>,
    pub roots: Vec<Entity>,
}

fn raw(frame: u32, view: Vec2) -> egui::RawInput {
    let mut raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(view.x, view.y),
        )),
        max_texture_side: Some(8192),
        time: Some(f64::from(frame) / 60.0),
        focused: true,
        ..Default::default()
    };
    raw.viewports
        .entry(raw.viewport_id)
        .or_default()
        .native_pixels_per_point = Some(1.0);
    raw
}

fn ui() -> Ui {
    let u = ui_sized(VIEW);
    u.ctx.run_ui(raw(0, VIEW), |_| {}).textures_delta.clear();
    u
}

fn ui_sized(view: Vec2) -> Ui {
    let mut place = place();
    place.run(
        r#"
        local rs = game:GetService("ReplicatedStorage")
        local pg = Instance.new("Folder")
        pg.Name = "PlayerGui"
        pg.Parent = rs
        S.pg = pg
        "#,
    );
    let rs = tree::find_child(&place.world, place.game, "ReplicatedStorage", false).unwrap();
    let pg = tree::find_child(&place.world, rs, "PlayerGui", false).unwrap();
    let ctx = egui::Context::default();
    gui::install_fonts(&ctx);
    Ui {
        place,
        ctx,
        view,
        camera: None,
        roots: vec![pg],
    }
}

impl Ui {
    fn run(&mut self, src: &str) {
        self.place.run(src);
    }

    /// One frame with `input`.
    fn frame_with(&mut self, input: GuiInput) -> GuiOutput {
        let frame = GuiFrame {
            viewport: self.view,
            top_inset: INSET,
            roots: self.roots.clone(),
            camera: self.camera,
            input,
        };
        let mut out = None;
        let place = &mut self.place;
        self.ctx
            .run_ui(raw(1, self.view), |ui| {
                out = Some(place.gui(ui.ctx(), &frame));
            })
            .textures_delta
            .clear();
        out.unwrap()
    }

    /// One frame painted into egui's output, texture changes included.
    fn painted(&mut self, n: u32) -> egui::FullOutput {
        let frame = GuiFrame {
            viewport: self.view,
            top_inset: 0.0,
            roots: self.roots.clone(),
            camera: self.camera,
            input: GuiInput::default(),
        };
        let place = &mut self.place;
        self.ctx.run_ui(raw(n, self.view), |ui| {
            let out = place.gui(ui.ctx(), &frame);
            gui::paint(ui.ctx(), out.shapes);
        })
    }

    fn frame(&mut self) -> GuiOutput {
        self.frame_with(GuiInput::default())
    }

    /// The printed lines of `src`.
    fn eval(&mut self, src: &str) -> Vec<String> {
        self.place.out();
        self.place.run(src);
        self.place.out()
    }

    /// `"x,y"` of a Vector2 expression over `S`.
    fn v2(&mut self, expr: &str) -> String {
        self.eval(&format!(
            "local v = {expr}\nprint(string.format('%g,%g', v.X, v.Y))"
        ))
        .join("|")
    }
}

/// A pointer at `(x, y)` in GUI space (below the top bar).
fn at(x: f32, y: f32) -> Option<Vec2> {
    Some(Vec2::new(x, y + INSET))
}

#[test]
fn position_size_and_anchor_resolve_against_the_parent() {
    let mut u = ui();
    u.run(
        r#"
        local sg = Instance.new("ScreenGui")
        sg.Parent = S.pg
        local f = Instance.new("Frame")
        f.Size = UDim2.new(0.5, 10, 0, 100)
        f.Position = UDim2.new(0.25, 0, 0.5, 0)
        f.AnchorPoint = Vector2.new(0.5, 0.5)
        f.Parent = sg
        local c = Instance.new("Frame")
        c.Size = UDim2.new(0.5, 0, 0.5, 0)
        c.Position = UDim2.fromOffset(10, 20)
        c.Parent = f
        S.sg, S.f, S.c = sg, f, c
        "#,
    );
    let out = u.frame();
    assert_eq!(out.drawn, 2);
    // The ScreenGui is the window less the top bar: 800 x 564.
    assert_eq!(u.v2("S.sg.AbsoluteSize"), "800,564");
    assert_eq!(u.v2("S.sg.AbsolutePosition"), "0,0");
    // 0.5 * 800 + 10 = 410 wide; centred on (200, 282).
    assert_eq!(u.v2("S.f.AbsoluteSize"), "410,100");
    assert_eq!(u.v2("S.f.AbsolutePosition"), "-5,232");
    assert_eq!(u.v2("S.c.AbsoluteSize"), "205,50");
    assert_eq!(u.v2("S.c.AbsolutePosition"), "5,252");
    // Drawn under the top bar the ScreenGui covers the window; GUI space
    // still starts below the bar.
    u.run("S.sg.IgnoreGuiInset = true");
    u.frame();
    assert_eq!(u.v2("S.sg.AbsoluteSize"), "800,600");
    assert_eq!(u.v2("S.sg.AbsolutePosition"), "0,-36");
    assert_eq!(u.v2("S.f.AbsolutePosition"), "-5,214");
    // A disabled ScreenGui draws nothing and keeps its values.
    u.run("S.sg.Enabled = false");
    assert_eq!(u.frame().drawn, 0);
}

#[test]
fn padding_and_constraints_shape_children() {
    let mut u = ui();
    u.run(
        r#"
        local sg = Instance.new("ScreenGui")
        sg.Parent = S.pg
        local f = Instance.new("Frame")
        f.Size = UDim2.fromOffset(400, 300)
        f.Parent = sg
        local pad = Instance.new("UIPadding")
        pad.PaddingLeft = UDim.new(0, 10)
        pad.PaddingTop = UDim.new(0, 10)
        pad.PaddingRight = UDim.new(0, 10)
        pad.PaddingBottom = UDim.new(0.1, 0)
        pad.Parent = f
        local fill = Instance.new("Frame")
        fill.Size = UDim2.fromScale(1, 1)
        fill.Parent = f
        local wide = Instance.new("Frame")
        wide.Size = UDim2.fromScale(1, 1)
        wide.Parent = f
        local ar = Instance.new("UIAspectRatioConstraint")
        ar.AspectRatio = 2
        ar.Parent = wide
        local narrow = Instance.new("Frame")
        narrow.Size = UDim2.fromScale(1, 1)
        narrow.Parent = f
        local sc = Instance.new("UISizeConstraint")
        sc.MaxSize = Vector2.new(100, math.huge)
        sc.MinSize = Vector2.new(0, 400)
        sc.Parent = narrow
        S.fill, S.wide, S.narrow = fill, wide, narrow
        "#,
    );
    u.frame();
    // 400 - 20 wide; 300 - 10 - 30 (10% of 300) tall.
    assert_eq!(u.v2("S.fill.AbsoluteSize"), "380,260");
    assert_eq!(u.v2("S.fill.AbsolutePosition"), "10,10");
    // Fit within 380 x 260 at 2:1.
    assert_eq!(u.v2("S.wide.AbsoluteSize"), "380,190");
    // At most 100 wide, at least 400 tall.
    assert_eq!(u.v2("S.narrow.AbsoluteSize"), "100,400");
}

#[test]
fn list_and_grid_layouts_place_siblings() {
    let mut u = ui();
    u.run(
        r#"
        local sg = Instance.new("ScreenGui")
        sg.Parent = S.pg
        local list = Instance.new("Frame")
        list.Size = UDim2.fromOffset(200, 300)
        list.Parent = sg
        local l = Instance.new("UIListLayout")
        l.FillDirection = Enum.FillDirection.Vertical
        l.HorizontalAlignment = Enum.HorizontalAlignment.Center
        l.SortOrder = Enum.SortOrder.LayoutOrder
        l.Padding = UDim.new(0, 5)
        l.Parent = list
        S.items = {}
        for i, order in ipairs({3, 1, 2}) do
            local f = Instance.new("Frame")
            f.Size = UDim2.fromOffset(50, 20)
            f.LayoutOrder = order
            f.Position = UDim2.fromOffset(999, 999) -- ignored under a layout
            f.Parent = list
            S.items[i] = f
        end
        S.list = l

        local grid = Instance.new("Frame")
        grid.Size = UDim2.fromOffset(170, 200)
        grid.Position = UDim2.fromOffset(300, 0)
        grid.Parent = sg
        local g = Instance.new("UIGridLayout")
        g.CellSize = UDim2.fromOffset(50, 50)
        g.CellPadding = UDim2.fromOffset(10, 10)
        g.SortOrder = Enum.SortOrder.Name
        g.Parent = grid
        S.cells = {}
        for i = 1, 5 do
            local f = Instance.new("Frame")
            f.Name = "c" .. i
            f.Size = UDim2.fromOffset(1, 1) -- replaced by the cell size
            f.Parent = grid
            S.cells[i] = f
        end
        S.grid = g
        "#,
    );
    u.frame();
    // Sorted by LayoutOrder: items[2], items[3], items[1]; centred in 200.
    assert_eq!(u.v2("S.items[2].AbsolutePosition"), "75,0");
    assert_eq!(u.v2("S.items[3].AbsolutePosition"), "75,25");
    assert_eq!(u.v2("S.items[1].AbsolutePosition"), "75,50");
    assert_eq!(u.v2("S.list.AbsoluteContentSize"), "50,70");
    // Three 50 px cells with 10 px gaps fit in 170: two rows for five.
    assert_eq!(u.v2("S.cells[1].AbsolutePosition"), "300,0");
    assert_eq!(u.v2("S.cells[3].AbsolutePosition"), "420,0");
    assert_eq!(u.v2("S.cells[4].AbsolutePosition"), "300,60");
    assert_eq!(u.v2("S.cells[5].AbsoluteSize"), "50,50");
    assert_eq!(u.v2("S.grid.AbsoluteCellCount"), "3,2");
    assert_eq!(u.v2("S.grid.AbsoluteCellSize"), "50,50");
    assert_eq!(u.v2("S.grid.AbsoluteContentSize"), "170,110");
    // A hidden item leaves no gap.
    u.run("S.items[2].Visible = false");
    u.frame();
    assert_eq!(u.v2("S.items[3].AbsolutePosition"), "75,0");
    assert_eq!(u.v2("S.list.AbsoluteContentSize"), "50,45");
}

#[test]
fn automatic_size_scroll_and_scale() {
    let mut u = ui();
    u.run(
        r#"
        local sg = Instance.new("ScreenGui")
        sg.Parent = S.pg
        local grow = Instance.new("Frame")
        grow.Size = UDim2.fromOffset(100, 10)
        grow.AutomaticSize = Enum.AutomaticSize.Y
        grow.Parent = sg
        local inner = Instance.new("Frame")
        inner.Position = UDim2.fromOffset(0, 30)
        inner.Size = UDim2.fromOffset(10, 40)
        inner.Parent = grow
        S.grow = grow

        local label = Instance.new("TextLabel")
        label.Text = "Hello, world"
        label.TextSize = 20
        label.Size = UDim2.fromOffset(0, 30)
        label.AutomaticSize = Enum.AutomaticSize.X
        label.Position = UDim2.fromOffset(0, 100)
        label.Parent = sg
        S.label = label

        local scroll = Instance.new("ScrollingFrame")
        scroll.Size = UDim2.fromOffset(100, 200)
        scroll.Position = UDim2.fromOffset(200, 0)
        scroll.CanvasSize = UDim2.fromOffset(0, 500)
        scroll.CanvasPosition = Vector2.new(0, 1000)
        scroll.Parent = sg
        local top = Instance.new("Frame")
        top.Size = UDim2.fromOffset(10, 10)
        top.Parent = scroll
        S.scroll, S.top = scroll, top

        local big = Instance.new("Frame")
        big.Size = UDim2.fromOffset(100, 50)
        big.Position = UDim2.fromOffset(400, 10)
        big.Parent = sg
        local s = Instance.new("UIScale")
        s.Scale = 2
        s.Parent = big
        local dot = Instance.new("Frame")
        dot.Size = UDim2.fromOffset(10, 10)
        dot.Position = UDim2.fromOffset(0, 10)
        dot.Parent = big
        S.big, S.dot = big, dot
        "#,
    );
    u.frame();
    // Grows to its child's bottom edge at 30 + 40.
    assert_eq!(u.v2("S.grow.AbsoluteSize"), "100,70");
    // The label is as wide as its text.
    let size = u.v2("S.label.AbsoluteSize");
    let bounds = u.v2("S.label.TextBounds");
    let w: f32 = size.split(',').next().unwrap().parse().unwrap();
    assert!(w > 60.0 && w < 200.0, "{size}");
    assert_eq!(
        size.split(',').next(),
        bounds.split(',').next(),
        "{size} vs {bounds}"
    );
    assert_eq!(u.eval("print(S.label.TextFits)"), ["true"]);
    // CanvasPosition is held to the canvas: 500 - 200.
    assert_eq!(u.v2("S.scroll.CanvasPosition"), "0,300");
    assert_eq!(u.v2("S.top.AbsolutePosition"), "200,-300");
    assert_eq!(u.v2("S.scroll.AbsoluteCanvasSize"), "100,500");
    assert_eq!(u.v2("S.scroll.AbsoluteWindowSize"), "100,200");
    // The wheel scrolls it back up: 50 px.
    u.frame_with(GuiInput {
        pointer: at(250.0, 100.0),
        scroll: Vec2::new(0.0, 50.0),
        ..Default::default()
    });
    assert_eq!(u.v2("S.scroll.CanvasPosition"), "0,250");
    // UIScale 2 about the anchor (its top-left corner).
    assert_eq!(u.v2("S.big.AbsoluteSize"), "200,100");
    assert_eq!(u.v2("S.big.AbsolutePosition"), "400,10");
    assert_eq!(u.v2("S.dot.AbsolutePosition"), "400,30");
    assert_eq!(u.v2("S.dot.AbsoluteSize"), "20,20");
}

#[test]
fn values_are_written_only_when_they_change() {
    let mut u = ui();
    u.run(
        r#"
        local sg = Instance.new("ScreenGui")
        sg.Parent = S.pg
        local f = Instance.new("Frame")
        f.Size = UDim2.fromOffset(10, 10)
        f.Parent = sg
        S.f = f
        S.n = 0
        f:GetPropertyChangedSignal("AbsoluteSize"):Connect(function()
            S.n += 1
        end)
        "#,
    );
    u.frame();
    u.frame();
    u.frame();
    assert_eq!(u.eval("print(S.n)"), ["1"]);
    u.run("S.f.Size = UDim2.fromOffset(20, 10)");
    u.frame();
    assert_eq!(u.eval("print(S.n)"), ["2"]);
}

#[test]
fn buttons_hover_click_and_activate() {
    let mut u = ui();
    u.run(
        r#"
        local sg = Instance.new("ScreenGui")
        sg.Parent = S.pg
        local b = Instance.new("TextButton")
        b.Size = UDim2.fromOffset(100, 50)
        b.Position = UDim2.fromOffset(10, 10)
        b.Text = "Go"
        b.Parent = sg
        local cover = Instance.new("Frame")
        cover.Size = UDim2.fromOffset(100, 50)
        cover.Position = UDim2.fromOffset(300, 10)
        cover.Active = true
        cover.Parent = sg
        local under = Instance.new("TextButton")
        under.Size = UDim2.fromOffset(100, 50)
        under.Position = UDim2.fromOffset(300, 10)
        under.ZIndex = 0
        under.Parent = sg
        S.log = {}
        local function log(s) table.insert(S.log, s) end
        for _, name in ipairs({"MouseEnter", "MouseLeave", "MouseButton1Down", "MouseButton1Up"}) do
            b[name]:Connect(function(x, y) log(name .. " " .. x .. " " .. y) end)
        end
        b.MouseButton1Click:Connect(function() log("Click") end)
        b.Activated:Connect(function(input, count) log("Activated " .. tostring(input) .. " " .. count) end)
        b.MouseButton2Click:Connect(function() log("Click2") end)
        under.MouseButton1Click:Connect(function() log("under") end)
        "#,
    );
    let press = |u: &mut Ui, p: Option<Vec2>, b1: bool, b2: bool| {
        u.frame_with(GuiInput {
            pointer: p,
            button1: b1,
            button2: b2,
            ..Default::default()
        })
    };
    let out = press(&mut u, at(20.0, 20.0), false, false);
    assert!(out.sinks_pointer);
    press(&mut u, at(21.0, 20.0), true, false);
    press(&mut u, at(22.0, 20.0), false, false);
    press(&mut u, at(22.0, 20.0), false, true);
    press(&mut u, at(22.0, 20.0), false, false);
    // Released away from the button: no click.
    press(&mut u, at(22.0, 20.0), true, false);
    let out = press(&mut u, at(500.0, 500.0), false, false);
    assert!(!out.sinks_pointer);
    // An Active frame on top takes the click from the button under it.
    let out = press(&mut u, at(320.0, 20.0), true, false);
    assert!(out.sinks_pointer);
    press(&mut u, at(320.0, 20.0), false, false);
    assert_eq!(
        u.eval("print(table.concat(S.log, '; '))"),
        [
            "MouseEnter 20 20; MouseButton1Down 21 20; MouseButton1Up 22 20; Click; \
          Activated nil 1; Click2; MouseButton1Down 22 20; MouseLeave 500 500"
        ]
    );
}

#[test]
fn text_boxes_focus_type_and_lose_focus() {
    let mut u = ui();
    u.run(
        r#"
        local sg = Instance.new("ScreenGui")
        sg.Parent = S.pg
        local t = Instance.new("TextBox")
        t.Size = UDim2.fromOffset(200, 30)
        t.Text = "old"
        t.Parent = sg
        S.t = t
        S.log = {}
        t.Focused:Connect(function() table.insert(S.log, "Focused") end)
        t.FocusLost:Connect(function(enter, input)
            table.insert(S.log, "FocusLost " .. tostring(enter) .. " " .. tostring(input))
        end)
        "#,
    );
    let click = GuiInput {
        pointer: at(10.0, 10.0),
        button1: true,
        ..Default::default()
    };
    let out = u.frame_with(click.clone());
    assert!(out.text_focus.is_some());
    assert_eq!(
        u.eval("print(S.t:IsFocused(), S.t.Text == '')"),
        ["true\ttrue"]
    );
    u.frame_with(GuiInput {
        pointer: at(10.0, 10.0),
        text: "hellp".into(),
        ..Default::default()
    });
    u.frame_with(GuiInput {
        backspace: 1,
        text: "o".into(),
        ..Default::default()
    });
    assert_eq!(u.eval("print(S.t.Text, S.t.CursorPosition)"), ["hello\t6"]);
    let out = u.frame_with(GuiInput {
        enter: true,
        ..Default::default()
    });
    assert!(out.text_focus.is_none());
    assert_eq!(
        u.eval("print(S.t:IsFocused(), S.t.CursorPosition)"),
        ["false\t-1"]
    );
    // CaptureFocus and a click elsewhere.
    u.run("S.t.ClearTextOnFocus = false; S.t:CaptureFocus()");
    assert_eq!(u.eval("print(S.t:IsFocused())"), ["true"]);
    u.frame();
    u.frame_with(GuiInput {
        pointer: at(500.0, 500.0),
        button1: true,
        ..Default::default()
    });
    assert_eq!(
        u.eval("print(S.t.Text, table.concat(S.log, '; '))"),
        ["hello\tFocused; FocusLost true nil; Focused; FocusLost false nil"]
    );
}

#[test]
fn font_and_font_face_are_one_value() {
    let mut u = ui();
    u.run(
        r#"
        local l = Instance.new("TextLabel")
        S.l = l
        "#,
    );
    assert_eq!(u.eval("print(S.l.Font)"), ["Enum.Font.SourceSans"]);
    u.run("S.l.Font = Enum.Font.GothamBold");
    assert_eq!(
        u.eval("print(S.l.FontFace.Family, S.l.FontFace.Weight)"),
        ["rbxasset://fonts/families/GothamSSm.json\tEnum.FontWeight.Bold"]
    );
    u.run("S.l.FontFace = Font.new('rbxasset://fonts/families/GothamSSm.json', Enum.FontWeight.Medium)");
    assert_eq!(u.eval("print(S.l.Font)"), ["Enum.Font.GothamMedium"]);
    u.run("S.l.FontFace = Font.new('rbxasset://fonts/families/Nunito.json', Enum.FontWeight.Thin)");
    assert_eq!(u.eval("print(S.l.Font)"), ["Enum.Font.Unknown"]);
}

#[test]
fn billboards_follow_their_adornee() {
    let mut u = ui();
    u.run(
        r#"
        local ws = game:GetService("Workspace")
        local anchor = Instance.new("Folder")
        anchor.Name = "Anchor"
        anchor.Parent = ws
        local bb = Instance.new("BillboardGui")
        bb.Adornee = anchor
        bb.Size = UDim2.new(2, 0, 1, 10)
        bb.MaxDistance = 25
        bb.Parent = S.pg
        local f = Instance.new("Frame")
        f.Size = UDim2.fromScale(1, 1)
        f.Parent = bb
        S.bb, S.f = bb, f
        "#,
    );
    let ws = tree::find_child(&u.place.world, u.place.game, "Workspace", false).unwrap();
    let anchor = tree::find_child(&u.place.world, ws, "Anchor", false).unwrap();
    let place_at = |u: &mut Ui, z: f32| {
        u.place
            .world
            .ecs
            .entity_mut(anchor)
            .insert(re_scene::GlobalTransform(glam::Affine3A::from_translation(
                Vec3::new(0.0, 0.0, z),
            )));
    };
    // 70 degrees vertical: 300 / tan(35°) = 428.45 px per stud at 1 stud.
    u.camera = Some(GuiCamera {
        position: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        fov_y: 70f32.to_radians(),
    });
    let focal = 300.0 / 35f32.to_radians().tan();
    let mut sizes = Vec::new();
    for z in [-10.0, -20.0] {
        place_at(&mut u, z);
        assert_eq!(u.frame().drawn, 1, "at {z}");
        let s = u.v2("S.bb.AbsoluteSize");
        let v: Vec<f32> = s.split(',').map(|x| x.parse().unwrap()).collect();
        let per = focal / -z;
        assert!((v[0] - 2.0 * per).abs() < 0.01, "{s} at {z}");
        assert!((v[1] - (per + 10.0)).abs() < 0.01, "{s} at {z}");
        // Centred on the screen; GUI space starts below the top bar.
        let p = u.v2("S.bb.AbsolutePosition");
        let p: Vec<f32> = p.split(',').map(|x| x.parse().unwrap()).collect();
        assert!((p[0] + v[0] / 2.0 - 400.0).abs() < 0.01, "{p:?}");
        assert!((p[1] + v[1] / 2.0 - (300.0 - INSET)).abs() < 0.01, "{p:?}");
        sizes.push(v[0]);
    }
    assert!((sizes[0] - 2.0 * sizes[1]).abs() < 0.01);
    // Beyond MaxDistance, and behind the camera, it is not drawn.
    place_at(&mut u, -30.0);
    assert_eq!(u.frame().drawn, 0);
    u.run("S.bb.MaxDistance = math.huge");
    assert_eq!(u.frame().drawn, 1);
    place_at(&mut u, 10.0);
    assert_eq!(u.frame().drawn, 0);
    // Without a camera none are.
    place_at(&mut u, -10.0);
    u.camera = None;
    assert_eq!(u.frame().drawn, 0);
}

#[test]
fn shapes_follow_draw_order() {
    let mut u = ui();
    u.run(
        r#"
        local back = Instance.new("ScreenGui")
        back.DisplayOrder = 1
        local front = Instance.new("ScreenGui")
        front.DisplayOrder = 2
        front.Parent = S.pg
        back.Parent = S.pg
        for i, sg in ipairs({back, front}) do
            local f = Instance.new("Frame")
            f.Size = UDim2.fromOffset(10, 10)
            f.BackgroundColor3 = Color3.new(i == 1 and 1 or 0, i == 2 and 1 or 0, 0)
            f.BorderSizePixel = 0
            f.Parent = sg
        end
        "#,
    );
    let out = u.frame();
    let fills: Vec<egui::Color32> = out
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Rect(r) => Some(r.fill),
            _ => None,
        })
        .collect();
    assert_eq!(fills, [egui::Color32::RED, egui::Color32::GREEN]);
}

/// A sheet of the UI features, painted through re_render: corners, strokes,
/// a gradient, rotation, the three substitute fonts, rich and scaled text, a
/// button, an image placeholder, a scrolled list and a grid. Compared with
/// `tests/fixtures/gui/goldens/<backend>/ui_sheet.png` at re_render's golden
/// tolerance; `RE_UPDATE_GOLDENS=1` rewrites it.
#[test]
fn golden_ui_sheet() {
    use re_render::imgdiff::{self, Tolerance};
    use re_render::{Capture, Gpu, Renderer, UiFrame};

    let backends = re_render::backends_from_env().expect("RE_BACKENDS");
    let gpu = match Gpu::headless(backends) {
        Ok(g) => g,
        Err(d) if std::env::var_os("CI").is_none() => {
            println!("skipped: no adapter: {}", d.render_human());
            return;
        }
        Err(d) => panic!("{}", d.render_human()),
    };
    let set = match gpu.backend() {
        re_render::wgpu::Backend::Vulkan => "vulkan",
        re_render::wgpu::Backend::Gl => "gl",
        other => {
            println!("no golden set for {other:?}");
            return;
        }
    };
    let view = Vec2::new(480.0, 320.0);
    let mut u = ui_sized(view);
    u.run(SHEET);
    let (w, h) = (view.x as u32, view.y as u32);
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let capture = Capture::new(&gpu.device, w, h).unwrap();
    // Frame 0 installs the fonts and frame 1 lays out with them; frame 2 is
    // the one compared.
    for n in 0..3 {
        renderer
            .render(
                &gpu.device,
                &gpu.queue,
                capture.view(),
                &re_types::render::RenderSnapshot::default(),
            )
            .unwrap();
        let mut output = u.painted(n);
        let frame = UiFrame::from_output(&u.ctx, &mut output, [w, h]);
        renderer
            .paint_ui(&gpu.device, &gpu.queue, capture.view(), &frame)
            .unwrap_or_else(|d| panic!("{}", d.render_human()));
    }
    assert_eq!(gpu.errors().count(), 0, "{:?}", gpu.errors().take());
    let image = capture.read(&gpu.device, &gpu.queue).unwrap();
    assert!(image.stats([0; 4]).distinct_colors > 100);

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = root.join("../../out/re_compat").join(set);
    image.save_png(&out.join("ui_sheet.png")).unwrap();
    let golden = root
        .join("tests/fixtures/gui/goldens")
        .join(set)
        .join("ui_sheet.png");
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
    println!("ui_sheet ({set}): {}", report.summary());
    if !report.matches() {
        let diff = imgdiff::diff_image(&image, &expected, Tolerance::default()).unwrap();
        diff.save_png(&out.join("ui_sheet.diff.png")).unwrap();
        panic!(
            "ui_sheet does not match its {set} golden: {} (actual and diff in {})",
            report.summary(),
            out.display()
        );
    }
}

const SHEET: &str = r##"
local function new(class, props, parent)
    local i = Instance.new(class)
    for k, v in pairs(props) do
        i[k] = v
    end
    i.Parent = parent
    return i
end
local px, rgb = UDim2.fromOffset, Color3.fromRGB
local sg = new("ScreenGui", {IgnoreGuiInset = true}, S.pg)
new("Frame", {Size = UDim2.fromScale(1, 1), BackgroundColor3 = rgb(30, 33, 40), BorderSizePixel = 0}, sg)

local card = new("Frame", {Position = px(16, 16), Size = px(140, 90), BackgroundColor3 = rgb(50, 120, 230)}, sg)
new("UICorner", {CornerRadius = UDim.new(0, 12)}, card)
new("UIStroke", {Thickness = 2, Color = rgb(255, 255, 255)}, card)

local grad = new("Frame", {Position = px(172, 16), Size = px(140, 90), BackgroundColor3 = rgb(255, 255, 255)}, sg)
new("UICorner", {CornerRadius = UDim.new(0, 8)}, grad)
new("UIGradient", {
    Color = ColorSequence.new({
        ColorSequenceKeypoint.new(0, rgb(230, 60, 60)),
        ColorSequenceKeypoint.new(0.5, rgb(240, 220, 60)),
        ColorSequenceKeypoint.new(1, rgb(60, 200, 90)),
    }),
    Rotation = 30,
}, grad)

local turned = new("Frame", {Position = px(340, 26), Size = px(110, 70), Rotation = 15,
    BackgroundColor3 = rgb(240, 140, 40), BorderSizePixel = 0}, sg)
new("TextLabel", {Size = UDim2.fromScale(1, 1), BackgroundTransparency = 1, Text = "Rotated",
    Font = Enum.Font.GothamBold, TextSize = 18, TextColor3 = rgb(20, 20, 20)}, turned)

new("TextLabel", {Position = px(16, 122), Size = px(220, 30), BackgroundTransparency = 1,
    Text = "Montserrat Bold", Font = Enum.Font.GothamBold, TextSize = 22,
    TextColor3 = rgb(255, 255, 255), TextXAlignment = Enum.TextXAlignment.Left}, sg)
new("TextLabel", {Position = px(16, 156), Size = px(220, 24), BackgroundTransparency = 1,
    RichText = true, Text = '<b>Bold</b> and <font color="#ff5050">red</font> &amp; plain',
    TextSize = 18, TextColor3 = rgb(230, 230, 230), TextXAlignment = Enum.TextXAlignment.Left}, sg)
new("TextLabel", {Position = px(16, 184), Size = px(220, 24), BackgroundTransparency = 1,
    Text = "local x = 42 -- mono", Font = Enum.Font.Code, TextSize = 16,
    TextColor3 = rgb(160, 220, 160), TextXAlignment = Enum.TextXAlignment.Left}, sg)
new("TextLabel", {Position = px(16, 212), Size = px(220, 40), BackgroundColor3 = rgb(70, 70, 90),
    Text = "Scaled", TextScaled = true, TextColor3 = rgb(255, 255, 255),
    TextStrokeTransparency = 0, TextStrokeColor3 = rgb(0, 0, 0), BorderSizePixel = 0}, sg)

local button = new("TextButton", {Position = px(252, 122), Size = px(120, 40),
    BackgroundColor3 = rgb(60, 180, 90), Text = "Button", TextSize = 18,
    TextColor3 = rgb(255, 255, 255), Font = Enum.Font.SourceSansBold}, sg)
new("UICorner", {CornerRadius = UDim.new(0.5, 0)}, button)
new("ImageLabel", {Position = px(384, 122), Size = px(80, 40), Image = "rbxassetid://1",
    ImageColor3 = rgb(170, 110, 230), BackgroundTransparency = 1}, sg)

local scroll = new("ScrollingFrame", {Position = px(252, 172), Size = px(212, 80),
    CanvasSize = px(0, 240), CanvasPosition = Vector2.new(0, 40), ScrollBarThickness = 6,
    ScrollBarImageColor3 = rgb(255, 255, 255), BackgroundColor3 = rgb(45, 48, 58), BorderSizePixel = 0}, sg)
new("UIListLayout", {SortOrder = Enum.SortOrder.LayoutOrder}, scroll)
for i = 1, 6 do
    new("TextLabel", {Size = UDim2.new(1, -6, 0, 40), LayoutOrder = i, BorderSizePixel = 0,
        BackgroundColor3 = i % 2 == 0 and rgb(60, 64, 78) or rgb(52, 55, 68),
        Text = "Row " .. i, TextSize = 16, TextColor3 = rgb(220, 220, 220)}, scroll)
end

local grid = new("Frame", {Position = px(16, 262), Size = px(448, 48), BackgroundTransparency = 1}, sg)
new("UIGridLayout", {CellSize = px(48, 48), CellPadding = px(8, 8)}, grid)
for i = 1, 8 do
    local cell = new("Frame", {BackgroundColor3 = Color3.fromHSV((i - 1) / 8, 0.7, 0.9), BorderSizePixel = 0}, grid)
    if i % 2 == 0 then
        new("UICorner", {CornerRadius = UDim.new(0, 10)}, cell)
    end
end
"##;

/// A class file with one `ColorSequence` property whose default is `default`.
fn sequence_class(default: &str) -> Result<(), String> {
    let file = format!(
        r#"{{"format": "real-engine/compat-classes", "version": 1, "classes": [{{
            "name": "SeqTarget", "superclass": "Instance", "creatable": true,
            "status": "implemented", "doc": "A test class.",
            "properties": [{{"name": "C", "type": "ColorSequence", "storage": {{"kind": "own"}},
                "default": {default}, "status": "implemented"}}]
        }}]}}"#
    );
    let parsed = re_compat::data::ClassFile::parse("seq.json", &file).map_err(|e| e.to_string())?;
    let mut b = re_compat::Bindings::standard().unwrap();
    b.classes(|c| c.classes("seq.json", parsed));
    re_compat::CompatSetup::new(b)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[test]
fn sequence_properties_store_lists_of_keypoints() {
    // The built-in UIGradient defaults read back as sequences, and a script
    // can write one.
    let mut u = ui();
    u.run(
        r#"
        local g = Instance.new("UIGradient")
        S.g = g
        "#,
    );
    assert_eq!(
        u.eval(
            "local k = S.g.Color.Keypoints print(#k, k[1].Time, k[2].Time, k[2].Value.R, \
             #S.g.Transparency.Keypoints)"
        ),
        ["2\t0\t1\t1\t2"]
    );
    u.run(
        "S.g.Color = ColorSequence.new({ColorSequenceKeypoint.new(0, Color3.new(1, 0, 0)), \
         ColorSequenceKeypoint.new(0.5, Color3.new(0, 1, 0)), \
         ColorSequenceKeypoint.new(1, Color3.new(0, 0, 1))})",
    );
    assert_eq!(
        u.eval("local k = S.g.Color.Keypoints print(#k, k[2].Time, k[2].Value.G)"),
        ["3\t0.5\t1"]
    );
    // A data file's default is checked when the registry is built.
    assert!(sequence_class("[[0, [1, 1, 1]], [1, [0, 0, 0]]]").is_ok());
    for bad in ["[[0]]", "[[0, \"red\"]]", "5"] {
        let err = sequence_class(bad)
            .err()
            .unwrap_or_else(|| panic!("{bad} was accepted"));
        assert!(err.contains("SeqTarget.C"), "{bad}: {err}");
    }
}

#[test]
fn root_scale_grid_aspect_and_focus_order() {
    let mut u = ui();
    u.run(
        r#"
        local sg = Instance.new("ScreenGui")
        sg.Parent = S.pg
        local sc = Instance.new("UIScale")
        sc.Scale = 2
        sc.Parent = sg
        local f = Instance.new("Frame")
        f.Size = UDim2.fromOffset(50, 40)
        f.Parent = sg
        local g = Instance.new("Frame")
        g.Size = UDim2.fromOffset(300, 300)
        g.Position = UDim2.fromOffset(0, 100)
        g.Parent = sg
        local grid = Instance.new("UIGridLayout")
        grid.CellSize = UDim2.fromOffset(100, 100)
        grid.Parent = g
        local ar = Instance.new("UIAspectRatioConstraint")
        ar.AspectRatio = 2
        ar.Parent = grid
        local cell = Instance.new("Frame")
        cell.Parent = g
        local a = Instance.new("TextBox")
        a.Size = UDim2.fromOffset(10, 10)
        a.Position = UDim2.fromOffset(0, 50)
        a.Parent = sg
        local b = a:Clone()
        b.Position = UDim2.fromOffset(20, 50)
        b.Parent = sg
        S.f, S.cell, S.a, S.b = f, cell, a, b
        "#,
    );
    u.frame();
    // The ScreenGui's UIScale doubles everything under it.
    assert_eq!(u.v2("S.f.AbsoluteSize"), "100,80");
    // The grid's constraint makes each cell 2:1 within 100 x 100 (scaled).
    assert_eq!(u.v2("S.cell.AbsoluteSize"), "200,100");
    // A box's ReleaseFocus after another box asked for focus leaves it be.
    u.run("S.a:CaptureFocus(); S.b:CaptureFocus(); S.a:ReleaseFocus()");
    u.frame();
    assert_eq!(
        u.eval("print(S.a:IsFocused(), S.b:IsFocused())"),
        ["false\ttrue"]
    );
    // A focused box that stops being drawn loses focus.
    u.run("S.b.Visible = false");
    let out = u.frame();
    assert!(out.text_focus.is_none());
    assert_eq!(u.eval("print(S.b:IsFocused())"), ["false"]);
}
