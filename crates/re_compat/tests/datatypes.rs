//! The datatypes (docs/M2_PLAN.md, WP10): the CFrame property tests against a
//! reference matrix model, hand-computed golden values, the Luau fixtures in
//! `tests/fixtures/datatypes/`, and the stored-form conversions.
//!
//! Every test is inside `mod datatypes`, so `cargo test -p re_compat
//! datatypes` selects them.

#[allow(clippy::needless_range_loop)] // Matrix code reads best with indices.
mod datatypes {
    use mlua::{Lua, Value as LuaValue};
    use re_compat::datatypes::{self, CFrame, Random};
    use re_compat::{ClassRegistryBuilder, EnumItem};
    use re_reflect::Value;

    /// A compat-style VM: safe libraries, the datatypes, frozen globals.
    fn vm(seed: u64) -> Lua {
        let lua = Lua::new();
        datatypes::install(&lua, &lua.globals(), seed).unwrap();
        lua.sandbox(true).unwrap();
        lua
    }

    fn run_fixture(name: &str, seed: u64) -> LuaValue {
        let path = format!(
            "{}/tests/fixtures/datatypes/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let src = std::fs::read_to_string(&path).unwrap();
        let lua = vm(seed);
        match lua
            .load(&src)
            .set_name(format!("@{name}"))
            .eval::<LuaValue>()
        {
            Ok(v) => v,
            Err(e) => panic!("{name}: {e}"),
        }
    }

    #[test]
    fn fixtures_pass() {
        for name in [
            "vector3.luau",
            "vector2.luau",
            "cframe.luau",
            "color.luau",
            "udim.luau",
            "sequences.luau",
            "misc.luau",
            "random.luau",
            "enums.luau",
            "typeof.luau",
        ] {
            run_fixture(name, 7);
        }
    }

    #[test]
    fn unseeded_random_follows_the_run_seed() {
        let a = run_fixture("random.luau", 99);
        let b = run_fixture("random.luau", 99);
        let c = run_fixture("random.luau", 100);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // ---- The reference model: f64 matrices, built independently. ----

    type M4 = [[f64; 4]; 4];

    fn m_from(cf: &CFrame) -> M4 {
        let mut m = [[0.0; 4]; 4];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = cf.r[i][j] as f64;
            }
            m[i][3] = cf.p[i] as f64;
        }
        m[3][3] = 1.0;
        m
    }

    fn m_mul(a: &M4, b: &M4) -> M4 {
        let mut o = [[0.0; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                o[i][j] = (0..4).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        o
    }

    fn m_apply(a: &M4, v: [f64; 3]) -> [f64; 3] {
        let mut o = [0.0; 3];
        for (i, x) in o.iter_mut().enumerate() {
            *x = a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2] + a[i][3];
        }
        o
    }

    fn rot(axis: usize, t: f64) -> M4 {
        let (s, c) = t.sin_cos();
        let mut m = [[0.0; 4]; 4];
        m[3][3] = 1.0;
        let (i, j) = match axis {
            0 => (1, 2),
            1 => (2, 0),
            _ => (0, 1),
        };
        m[axis][axis] = 1.0;
        m[i][i] = c;
        m[j][j] = c;
        m[i][j] = -s;
        m[j][i] = s;
        m
    }

    fn max_diff(a: &M4, b: &M4) -> f64 {
        let mut d: f64 = 0.0;
        for i in 0..3 {
            for j in 0..4 {
                d = d.max((a[i][j] - b[i][j]).abs());
            }
        }
        d
    }

    fn vdiff(a: [f32; 3], b: [f64; 3]) -> f64 {
        (0..3)
            .map(|i| (a[i] as f64 - b[i]).abs())
            .fold(0.0, f64::max)
    }

    struct Gen(Random);

    impl Gen {
        fn num(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.0.next_number()
        }
        fn angles(&mut self) -> [f64; 3] {
            // Away from the gimbal-lock band so the Euler round trip is
            // well-conditioned.
            [
                self.num(-3.1, 3.1),
                self.num(-1.5, 1.5),
                self.num(-3.1, 3.1),
            ]
        }
        fn cframe(&mut self) -> (CFrame, M4) {
            let [a, b, c] = self.angles();
            let p = [
                self.num(-10.0, 10.0),
                self.num(-10.0, 10.0),
                self.num(-10.0, 10.0),
            ];
            let cf = CFrame::from_position([p[0] as f32, p[1] as f32, p[2] as f32])
                * CFrame::from_euler_xyz(a as f32, b as f32, c as f32);
            let mut m = m_mul(&m_mul(&rot(0, a), &rot(1, b)), &rot(2, c));
            for i in 0..3 {
                m[i][3] = p[i];
            }
            (cf, m)
        }
    }

    const CASES: usize = 10_000;
    const TOLERANCE: f64 = 1e-5;

    #[test]
    fn cframe_matches_the_reference_model() {
        let mut g = Gen(Random::new(0x5eed));
        let mut worst = [0.0f64; 6];
        for _ in 0..CASES {
            let (a, ma) = g.cframe();
            let (b, mb) = g.cframe();
            // Construction: Angles is Rx * Ry * Rz, translated.
            worst[0] = worst[0].max(max_diff(&m_from(&a), &ma));
            // Composition.
            worst[1] = worst[1].max(max_diff(&m_from(&(a * b)), &m_mul(&ma, &mb)));
            // Inverse: a * a^-1 is the identity.
            let id = m_from(&(a * a.inverse()));
            let mut ident = [[0.0; 4]; 4];
            for (i, row) in ident.iter_mut().enumerate() {
                row[i] = 1.0;
            }
            worst[2] = worst[2].max(max_diff(&id, &ident));
            // Point and vector round trips, and the world transform itself.
            let v = [g.num(-10.0, 10.0), g.num(-10.0, 10.0), g.num(-10.0, 10.0)];
            let vf = [v[0] as f32, v[1] as f32, v[2] as f32];
            worst[3] = worst[3].max(vdiff(a.point_to_world(vf), m_apply(&ma, v)));
            worst[3] = worst[3].max(vdiff(a.point_to_object(a.point_to_world(vf)), v));
            worst[3] = worst[3].max(vdiff(a.vector_to_object(a.vector_to_world(vf)), v));
            let rel = a.inverse() * b;
            worst[3] = worst[3].max(max_diff(&m_from(&(a * rel)), &mb));
            // Euler angles, both orders.
            let [x, y, z] = g.angles();
            let (rx, ry, rz) = CFrame::from_euler_xyz(x as f32, y as f32, z as f32).to_euler_xyz();
            let d = [rx as f64 - x, ry as f64 - y, rz as f64 - z];
            worst[4] = worst[4].max(d.iter().map(|d| d.abs()).fold(0.0, f64::max));
            let yxz = CFrame::from_euler_yxz(y as f32, x as f32, z as f32);
            let my = m_mul(&m_mul(&rot(1, x), &rot(0, y)), &rot(2, z));
            worst[5] = worst[5].max(max_diff(&m_from(&yxz), &my));
            let (rx, ry, rz) = yxz.to_euler_yxz();
            let d = [rx as f64 - y, ry as f64 - x, rz as f64 - z];
            worst[5] = worst[5].max(d.iter().map(|d| d.abs()).fold(0.0, f64::max));
        }
        // Absolute errors, with positions up to 10 and angles in radians.
        let names = [
            "angles",
            "compose",
            "inverse",
            "spaces",
            "euler xyz",
            "euler yxz",
        ];
        for (n, w) in names.iter().zip(worst) {
            assert!(w < TOLERANCE, "{n}: worst error {w:e}");
        }
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-6)
    }

    #[test]
    fn cframe_golden_values() {
        // Angles applies Z first: rotating X by 90° about Z then 90° about Y.
        let cf = CFrame::from_euler_xyz(
            0.0,
            std::f32::consts::FRAC_PI_2,
            std::f32::consts::FRAC_PI_2,
        );
        // Rz(90) X = Y; Ry(90) Y = Y.
        assert!(close(cf.point_to_world([1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
        // Rz(90) Y = -X; Ry(90) (-X) = +Z.
        assert!(close(cf.point_to_world([0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));

        // lookAt with the up vector parallel to the look direction.
        let up = CFrame::look_at([0.0; 3], [0.0, 5.0, 0.0], [0.0, 1.0, 0.0]);
        assert!(close(up.look_vector(), [0.0, 1.0, 0.0]));
        assert!(close(up.x_vector(), [1.0, 0.0, 0.0])); // Y x Z
        assert!(close(up.y_vector(), [0.0, 0.0, 1.0]));
        assert!(up.is_orthonormal());
        let down = CFrame::look_at([1.0, 2.0, 3.0], [1.0, -5.0, 3.0], [0.0, 1.0, 0.0]);
        assert!(close(down.look_vector(), [0.0, -1.0, 0.0]));
        assert!(close(down.x_vector(), [-1.0, 0.0, 0.0]));
        assert!(down.is_orthonormal());
        assert_eq!(down.p, [1.0, 2.0, 3.0]);
        // A plain lookAt: from the origin towards +X.
        let east = CFrame::look_at([0.0; 3], [3.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert!(close(east.look_vector(), [1.0, 0.0, 0.0]));
        assert!(close(east.x_vector(), [0.0, 0.0, 1.0]));
        assert!(close(east.y_vector(), [0.0, 1.0, 0.0]));

        // fromMatrix with a non-orthonormal input keeps X's direction.
        let m = CFrame::from_parts(
            [0.0; 3],
            [[2.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 3.0]],
        );
        assert!(m.is_orthonormal());
        assert!(close(m.x_vector(), [1.0, 0.0, 0.0]));
        assert!(close(m.y_vector(), [0.0, 1.0, 0.0]));
        assert!(close(m.z_vector(), [0.0, 0.0, 1.0]));
        let skew = CFrame::from_parts(
            [0.0; 3],
            [[0.0, 1.0, 0.0], [3.0, 1.0, 0.0], [0.0, 0.0, 0.5]],
        );
        // X = (0, 3, 0) → +Y; Y = (1, 1, 0) minus its +Y part → +X; Z = X × Y = -Z.
        assert!(close(skew.x_vector(), [0.0, 1.0, 0.0]));
        assert!(close(skew.y_vector(), [1.0, 0.0, 0.0]));
        assert!(close(skew.z_vector(), [0.0, 0.0, -1.0]));
        // An orthonormal input is kept bit for bit.
        let r = CFrame::from_euler_xyz(0.1, 0.2, 0.3);
        assert_eq!(CFrame::from_parts(r.p, r.r), r);

        // Lerp at 0, 0.5 and 1.
        let a = CFrame::from_position([0.0, 0.0, 0.0]);
        let b = CFrame::from_position([10.0, 0.0, 0.0])
            * CFrame::from_euler_xyz(0.0, std::f32::consts::FRAC_PI_2, 0.0);
        assert_eq!(a.lerp(&b, 0.0), a);
        assert!(a.lerp(&b, 1.0).fuzzy_eq(&b, 1e-6));
        let half = CFrame::from_position([5.0, 0.0, 0.0])
            * CFrame::from_euler_xyz(0.0, std::f32::consts::FRAC_PI_4, 0.0);
        assert!(a.lerp(&b, 0.5).fuzzy_eq(&half, 1e-6));
    }

    #[test]
    fn stored_forms_round_trip() {
        // Building the registry registers the record types.
        ClassRegistryBuilder::builtin()
            .unwrap()
            .build_data_only()
            .unwrap();
        let lua = vm(1);
        let cases = [
            (
                "CFrame",
                "CFrame.new(1, 2, 3) * CFrame.Angles(0.1, 0.2, 0.3)",
            ),
            ("Vector3", "Vector3.new(0.1, 2, 3)"),
            ("Vector2", "Vector2.new(4, 5)"),
            ("Color3", "Color3.fromRGB(10, 20, 30)"),
            ("UDim", "UDim.new(0.5, -3)"),
            ("UDim2", "UDim2.new(0.1, 2, 0.3, 4)"),
            ("Rect", "Rect.new(1, 2, 3, 4)"),
            ("NumberRange", "NumberRange.new(1, 2)"),
            (
                "NumberSequence",
                "NumberSequence.new({NumberSequenceKeypoint.new(0, 1), NumberSequenceKeypoint.new(0.5, 2, 0.25), NumberSequenceKeypoint.new(1, 3)})",
            ),
            (
                "ColorSequence",
                "ColorSequence.new(Color3.new(1, 0, 0), Color3.new(0, 0, 1))",
            ),
            (
                "PhysicalProperties",
                "PhysicalProperties.new(1, 0.5, 0.25, 2, 3)",
            ),
            (
                "Font",
                "Font.new('rbxasset://fonts/families/Example.json', Enum.FontWeight.Bold)",
            ),
            ("Enum.Material", "Enum.Material.Neon"),
            ("number", "0.1"),
            ("bool", "true"),
            ("string", "'hi'"),
        ];
        let eq: mlua::Function = lua
            .load("return function(a, b) return a == b end")
            .eval()
            .unwrap();
        for (ty, src) in cases {
            let v: LuaValue = lua.load(src).eval().unwrap();
            let stored =
                datatypes::from_lua(&lua, ty, v.clone()).unwrap_or_else(|e| panic!("{ty}: {e}"));
            let back = datatypes::to_lua(&lua, ty, stored.clone()).unwrap();
            assert!(
                eq.call::<bool>((v, back.clone())).unwrap(),
                "{ty}: round trip changed the value"
            );
            let again = datatypes::from_lua(&lua, ty, back).unwrap();
            assert_eq!(stored, again, "{ty}");
        }
        // The stored forms.
        let v: LuaValue = lua.load("Color3.new(0.5, 2, -1)").eval().unwrap();
        match datatypes::from_lua(&lua, "Color3", v).unwrap() {
            Value::Color(c) => assert_eq!((c.r, c.g, c.b, c.a), (0.5, 2.0, -1.0, 1.0)),
            other => panic!("{other:?}"),
        }
        let v: LuaValue = lua.load("UDim2.new(0.5, 10, 1, -20)").eval().unwrap();
        match datatypes::from_lua(&lua, "UDim2", v).unwrap() {
            Value::Record(id, values) => {
                assert_eq!(id.name(), "UDim2");
                assert_eq!(
                    values,
                    vec![
                        Value::Float(0.5),
                        Value::Int(10),
                        Value::Float(1.0),
                        Value::Int(-20)
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
        // Enum properties accept an item, its name or its number.
        let neon = EnumItem::named("Material", "Neon");
        for src in ["Enum.Material.Neon", "'Neon'", &neon.value().to_string()] {
            let v: LuaValue = lua.load(src).eval().unwrap();
            assert_eq!(
                datatypes::from_lua(&lua, "Enum.Material", v).unwrap(),
                Value::Enum("Neon".into())
            );
        }
        for bad in ["Enum.KeyCode.E", "'Nope'", "true", "123456"] {
            let v: LuaValue = lua.load(bad).eval().unwrap();
            assert!(
                datatypes::from_lua(&lua, "Enum.Material", v).is_err(),
                "{bad}"
            );
        }
        // A wrong type is refused with RE3006.
        let v: LuaValue = lua.load("Vector3.zero").eval().unwrap();
        let err = datatypes::from_lua(&lua, "CFrame", v)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("RE3006") && err.contains("CFrame expected, got Vector3"),
            "{err}"
        );
        // An enum read gives the VM's single item object.
        let item = datatypes::to_lua(&lua, "Enum.Material", Value::Enum("Neon".into())).unwrap();
        let same: bool = lua
            .load("return function(x) return rawequal(x, Enum.Material.Neon) end")
            .eval::<mlua::Function>()
            .unwrap()
            .call(item)
            .unwrap();
        assert!(same);
    }
}
