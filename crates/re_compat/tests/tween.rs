//! WP15: TweenService as scripts see it, in a client VM.

use wp15::place;

mod wp15;

/// Hand-computed values of every easing pair at 0, 25, 50, 75 and 100 %.
const TABLE: &[(&str, &str, [f64; 5])] = &[
    ("Linear", "In", [0.0, 0.25, 0.5, 0.75, 1.0]),
    ("Linear", "Out", [0.0, 0.25, 0.5, 0.75, 1.0]),
    ("Linear", "InOut", [0.0, 0.25, 0.5, 0.75, 1.0]),
    (
        "Sine",
        "In",
        [0.0, 0.076120467489, 0.292893218813, 0.617316567635, 1.0],
    ),
    (
        "Sine",
        "Out",
        [
            0.0,
            0.382683432365,
            std::f64::consts::FRAC_1_SQRT_2,
            0.923879532511,
            1.0,
        ],
    ),
    (
        "Sine",
        "InOut",
        [0.0, 0.146446609407, 0.5, 0.853553390593, 1.0],
    ),
    (
        "Back",
        "In",
        [0.0, -0.0641365625, -0.0876975, 0.1825903125, 1.0],
    ),
    (
        "Back",
        "Out",
        [0.0, 0.8174096875, 1.0876975, 1.0641365625, 1.0],
    ),
    ("Back", "InOut", [0.0, -0.04384875, 0.5, 1.04384875, 1.0]),
    ("Quad", "In", [0.0, 0.0625, 0.25, 0.5625, 1.0]),
    ("Quad", "Out", [0.0, 0.4375, 0.75, 0.9375, 1.0]),
    ("Quad", "InOut", [0.0, 0.125, 0.5, 0.875, 1.0]),
    ("Quart", "In", [0.0, 0.00390625, 0.0625, 0.31640625, 1.0]),
    ("Quart", "Out", [0.0, 0.68359375, 0.9375, 0.99609375, 1.0]),
    ("Quart", "InOut", [0.0, 0.03125, 0.5, 0.96875, 1.0]),
    (
        "Quint",
        "In",
        [0.0, 0.0009765625, 0.03125, 0.2373046875, 1.0],
    ),
    (
        "Quint",
        "Out",
        [0.0, 0.7626953125, 0.96875, 0.9990234375, 1.0],
    ),
    ("Quint", "InOut", [0.0, 0.015625, 0.5, 0.984375, 1.0]),
    ("Bounce", "In", [0.0, 0.02734375, 0.234375, 0.52734375, 1.0]),
    (
        "Bounce",
        "Out",
        [0.0, 0.47265625, 0.765625, 0.97265625, 1.0],
    ),
    ("Bounce", "InOut", [0.0, 0.1171875, 0.5, 0.8828125, 1.0]),
    (
        "Elastic",
        "In",
        [0.0, -0.005524271728, -0.015625, 0.088388347648, 1.0],
    ),
    (
        "Elastic",
        "Out",
        [0.0, 0.911611652352, 1.015625, 1.005524271728, 1.0],
    ),
    ("Elastic", "InOut", [0.0, -0.0078125, 0.5, 1.0078125, 1.0]),
    (
        "Exponential",
        "In",
        [0.0, 0.005524271728, 0.03125, 0.176776695297, 1.0],
    ),
    (
        "Exponential",
        "Out",
        [0.0, 0.823223304703, 0.96875, 0.994475728272, 1.0],
    ),
    ("Exponential", "InOut", [0.0, 0.015625, 0.5, 0.984375, 1.0]),
    (
        "Circular",
        "In",
        [0.0, 0.031754163448, 0.133974596216, 0.338562172234, 1.0],
    ),
    (
        "Circular",
        "Out",
        [0.0, 0.661437827766, 0.866025403784, 0.968245836552, 1.0],
    ),
    (
        "Circular",
        "InOut",
        [0.0, 0.066987298108, 0.5, 0.933012701892, 1.0],
    ),
    ("Cubic", "In", [0.0, 0.015625, 0.125, 0.421875, 1.0]),
    ("Cubic", "Out", [0.0, 0.578125, 0.875, 0.984375, 1.0]),
    ("Cubic", "InOut", [0.0, 0.0625, 0.5, 0.9375, 1.0]),
];

#[test]
fn every_easing_pair_at_quarters() {
    let mut p = place();
    // GetValue, and a one-second tween of a number stepped in quarters.
    let mut src = String::from(
        "local TS = game:GetService('TweenService')\nlocal function near(a, b) return math.abs(a - b) < 1e-6 end\n",
    );
    for (style, dir, v) in TABLE {
        src.push_str(&format!(
            "do\n  local want = {{{}, {}, {}, {}, {}}}\n  \
             for i, a in {{0, 0.25, 0.5, 0.75, 1}} do\n    \
             local got = TS:GetValue(a, Enum.EasingStyle.{style}, Enum.EasingDirection.{dir})\n    \
             assert(near(got, want[i]), `GetValue {style} {dir} {{a}}: {{got}}`)\n  end\n  \
             local t = Instance.new('TweenTarget')\n  t.Name = '{style}{dir}'\n  t.Parent = workspace\n  \
             TS:Create(t, TweenInfo.new(1, Enum.EasingStyle.{style}, Enum.EasingDirection.{dir}), {{N = 100}}):Play()\nend\n",
            v[0], v[1], v[2], v[3], v[4]
        ));
    }
    p.run(&src);
    for q in 1..=4 {
        p.frame(0.25);
        let mut check = String::new();
        for (style, dir, v) in TABLE {
            check.push_str(&format!(
                "do local n = workspace['{style}{dir}'].N assert(math.abs(n - {}) < 1e-6, `{style} {dir} at {q}/4: {{n}}`) end\n",
                v[q] * 100.0
            ));
        }
        p.run(&check);
    }
}

#[test]
fn every_tweenable_type_at_half_way() {
    let mut p = place();
    p.run(
        r#"
        local TS = game:GetService("TweenService")
        local t = Instance.new("TweenTarget", workspace)
        t.N, t.I = 0, 0
        t.V = Vector3.new(0, 0, 0)
        t.C = Color3.new(0, 0, 0)
        t.U = UDim2.new(0, 0, 0, 0)
        t.F = CFrame.new(0, 0, 0)
        t.V2 = Vector2.new(0, 0)
        t.B = false
        S.tw = TS:Create(t, TweenInfo.new(2, Enum.EasingStyle.Linear), {
            N = 10, I = 5, V = Vector3.new(2, 4, 6), C = Color3.new(1, 0.5, 0),
            U = UDim2.new(1, 100, 0.5, -10), F = CFrame.new(10, 0, 0) * CFrame.Angles(0, math.pi / 2, 0),
            V2 = Vector2.new(8, -8), B = true,
        })
        S.tw:Play()
        "#,
    );
    p.frame(1.0);
    p.run("print(S.tw.PlaybackState.Name, workspace.TweenTarget.N)");
    eprintln!("{:?}", p.out());
    p.run(
        r#"
        local t = workspace.TweenTarget
        local function near(a, b) return math.abs(a - b) < 1e-4 end
        assert(near(t.N, 5), t.N)
        assert(t.I == 3, t.I) -- 2.5 rounds to the nearest whole number
        assert(t.V:FuzzyEq(Vector3.new(1, 2, 3), 1e-4), tostring(t.V))
        assert(near(t.C.R, 0.5) and near(t.C.G, 0.25) and near(t.C.B, 0), tostring(t.C))
        assert(t.U == UDim2.new(0.5, 50, 0.25, -5), tostring(t.U))
        assert(t.F.Position:FuzzyEq(Vector3.new(5, 0, 0), 1e-4), tostring(t.F))
        local _, y = t.F:ToEulerAnglesYXZ()
        assert(near(y, math.pi / 4), y)
        assert(t.V2 == Vector2.new(4, -4), tostring(t.V2))
        assert(t.B == false, "a boolean switches at the end")
        "#,
    );
    p.frame(1.0);
    p.run(
        r#"
        local t = workspace.TweenTarget
        assert(t.N == 10 and t.I == 5 and t.B == true)
        assert(t.U == UDim2.new(1, 100, 0.5, -10))
        "#,
    );
}

#[test]
fn states_completed_pause_and_cancel() {
    let mut p = place();
    p.run(
        r#"
        local TS = game:GetService("TweenService")
        local t = Instance.new("TweenTarget", workspace)
        local tw = TS:Create(t, TweenInfo.new(1, Enum.EasingStyle.Linear), { N = 10 })
        S.tw = tw
        assert(tw.PlaybackState == Enum.PlaybackState.Begin)
        assert(tw.Instance == t and tw.ClassName == "Tween" and tw.Parent == nil)
        assert(tw:IsA("TweenBase"))
        tw.Completed:Connect(function(state) print("completed " .. state.Name) end)
        tw:Play()
        assert(tw.PlaybackState == Enum.PlaybackState.Playing)
        "#,
    );
    p.frame(0.5);
    p.run(
        r#"
        local tw = S.tw
        tw:Pause()
        assert(tw.PlaybackState == Enum.PlaybackState.Paused)
        "#,
    );
    p.frame(10.0);
    p.run("assert(math.abs(workspace.TweenTarget.N - 5) < 1e-9) S.tw:Play()");
    p.frame(0.25);
    p.run("assert(math.abs(workspace.TweenTarget.N - 7.5) < 1e-9) S.tw:Cancel()");
    assert_eq!(p.out(), vec!["completed Cancelled"]);
    p.frame(1.0);
    p.run(
        r#"
        local t = workspace.TweenTarget
        assert(math.abs(t.N - 7.5) < 1e-9, "Cancel keeps the value")
        assert(S.tw.PlaybackState == Enum.PlaybackState.Cancelled)
        S.tw:Play() -- starts over from 7.5
        "#,
    );
    p.frame(0.5);
    p.run("assert(math.abs(workspace.TweenTarget.N - 8.75) < 1e-9, workspace.TweenTarget.N)");
    p.frame(0.5);
    p.run("assert(workspace.TweenTarget.N == 10 and S.tw.PlaybackState == Enum.PlaybackState.Completed)");
    assert_eq!(p.out(), vec!["completed Completed"]);
}

#[test]
fn delay_repeat_and_reverse() {
    let mut p = place();
    p.run(
        r#"
        local TS = game:GetService("TweenService")
        local t = Instance.new("TweenTarget", workspace)
        -- 1 s delay, 1 s out, 1 s back, twice.
        S.tw = TS:Create(t, TweenInfo.new(1, Enum.EasingStyle.Linear, Enum.EasingDirection.In, 1, true, 1), { N = 10 })
        S.tw.Completed:Connect(function(s) print(s.Name) end)
        S.tw:Play()
        assert(S.tw.PlaybackState == Enum.PlaybackState.Delayed)
        "#,
    );
    let mut seen = Vec::new();
    for _ in 0..12 {
        p.frame(0.5);
        p.run("print(workspace.TweenTarget.N .. ' ' .. S.tw.PlaybackState.Name)");
        seen.extend(p.out());
    }
    let want: Vec<String> = [
        "0 Delayed",
        "0 Playing",
        "5 Playing",
        "10 Playing",
        "5 Playing",
        "0 Delayed",
        "0 Delayed",
        "0 Playing",
        "5 Playing",
        "10 Playing",
        "5 Playing",
        "Completed",
        "0 Completed",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(seen, want);
}

#[test]
fn a_newer_tween_of_a_property_cancels_the_older() {
    let mut p = place();
    p.run(
        r#"
        local TS = game:GetService("TweenService")
        local t = Instance.new("TweenTarget", workspace)
        local a = TS:Create(t, TweenInfo.new(1, Enum.EasingStyle.Linear), { N = 10, I = 10 })
        local b = TS:Create(t, TweenInfo.new(1, Enum.EasingStyle.Linear), { N = -10 })
        local other = Instance.new("TweenTarget", workspace)
        local c = TS:Create(other, TweenInfo.new(1, Enum.EasingStyle.Linear), { N = 10 })
        a.Completed:Connect(function(s) print("a " .. s.Name) end)
        a:Play() c:Play()
        b:Play()
        assert(a.PlaybackState == Enum.PlaybackState.Cancelled)
        assert(c.PlaybackState == Enum.PlaybackState.Playing, "another instance is not affected")
        S.b = b
        "#,
    );
    p.frame(0.5);
    assert_eq!(p.out(), vec!["a Cancelled"]);
    p.run("assert(math.abs(workspace.TweenTarget.N + 5) < 1e-9)");
}

#[test]
fn refused_goals() {
    let mut p = place();
    p.run("S.t = Instance.new('TweenTarget', workspace)");
    let ts = "local TS = game:GetService('TweenService') local i = TweenInfo.new(1)";
    let e = p.fails(&format!(
        "{ts} TS:Create(S.t, i, {{ M = Enum.Material.Neon }})"
    ));
    assert!(e.contains("RE9352") && e.contains("TweenTarget.M"), "{e}");
    let e = p.fails(&format!("{ts} TS:Create(S.t, i, {{ R = 1 }})"));
    assert!(e.contains("RE9005"), "{e}");
    let e = p.fails(&format!("{ts} TS:Create(S.t, i, {{ Nope = 1 }})"));
    assert!(e.contains("RE9001"), "{e}");
    let e = p.fails(&format!("{ts} TS:Create(S.t, i, {{ N = 'x' }})"));
    assert!(e.contains("bad goal for TweenTarget.N"), "{e}");
    let e = p.fails(&format!("{ts} TS:Create(S.t, 1, {{ N = 1 }})"));
    assert!(e.contains("TweenInfo"), "{e}");
}

#[test]
fn a_destroyed_target_stops_its_tween_and_finished_tweens_are_collected() {
    let mut p = place();
    p.run(
        r#"
        local TS = game:GetService("TweenService")
        local t = Instance.new("TweenTarget", workspace)
        local tw = TS:Create(t, TweenInfo.new(1), { N = 10 })
        tw.Completed:Connect(function() print("never") end)
        tw:Play()
        t:Destroy()
        for i = 1, 20 do
            local x = Instance.new("TweenTarget", workspace)
            TS:Create(x, TweenInfo.new(0.1), { N = i }):Play()
        end
        "#,
    );
    assert_eq!(p.lua(re_compat::tween::count), 21);
    for _ in 0..130 {
        p.frame(1.0 / 60.0);
    }
    assert!(p.out().is_empty());
    assert_eq!(p.lua(re_compat::tween::count), 0);
    p.run("for _, x in workspace:GetChildren() do assert(x.N > 0) end");
}

#[test]
fn zero_length_tween_completes_on_play() {
    let mut p = place();
    p.run(
        r#"
        local t = Instance.new("TweenTarget", workspace)
        local tw = game:GetService("TweenService"):Create(t, TweenInfo.new(0), { N = 3 })
        tw:Play()
        assert(t.N == 3 and tw.PlaybackState == Enum.PlaybackState.Completed)
        "#,
    );
}
