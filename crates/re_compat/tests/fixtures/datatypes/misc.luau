local t = TweenInfo.new()
assert(typeof(t) == "TweenInfo")
assert(t.Time == 1 and t.EasingStyle == Enum.EasingStyle.Quad and t.EasingDirection == Enum.EasingDirection.Out)
assert(t.RepeatCount == 0 and t.Reverses == false and t.DelayTime == 0)
local t2 = TweenInfo.new(0.25, Enum.EasingStyle.Sine, Enum.EasingDirection.InOut, 2, true, 0.1)
assert(t2.Time == 0.25 and t2.EasingStyle == Enum.EasingStyle.Sine and t2.RepeatCount == 2 and t2.Reverses and t2.DelayTime == 0.1)
assert(TweenInfo.new(0.5, "Linear").EasingStyle == Enum.EasingStyle.Linear)
assert(not pcall(TweenInfo.new, 1, Enum.EasingDirection.In))

local pp = PhysicalProperties.new(0.9, 0.3, 0, 1, 100)
assert(typeof(pp) == "PhysicalProperties")
assert(math.abs(pp.Density - 0.9) < 1e-6 and pp.Elasticity == 0 and pp.ElasticityWeight == 100)
assert(PhysicalProperties.new(1, 2, 0.5).FrictionWeight == 1)
local metal = PhysicalProperties.new(Enum.Material.Metal)
assert(metal.Density > 7 and metal.Density < 8)

local f = Font.new("rbxasset://fonts/families/Example.json", Enum.FontWeight.Bold)
assert(typeof(f) == "Font" and f.Weight == Enum.FontWeight.Bold and f.Style == Enum.FontStyle.Normal and f.Bold)
assert(Font.new("x").Weight == Enum.FontWeight.Regular and not Font.new("x").Bold)
assert(Font.fromName("Example").Family == "rbxasset://fonts/families/Example.json")

local rp = RaycastParams.new()
assert(typeof(rp) == "RaycastParams")
assert(rp.FilterType == Enum.RaycastFilterType.Exclude and #rp.FilterDescendantsInstances == 0)
assert(rp.CollisionGroup == "Default" and rp.IgnoreWater == false and rp.RespectCanCollide == false)
rp.FilterType = Enum.RaycastFilterType.Include
rp.IgnoreWater = true
rp.CollisionGroup = "Debris"
assert(rp.FilterType == Enum.RaycastFilterType.Include and rp.IgnoreWater and rp.CollisionGroup == "Debris")
assert(not pcall(function()
	rp.FilterType = Enum.Material.Plastic
end))
assert(not pcall(function()
	(rp :: any).Bogus = 1
end))

local op = OverlapParams.new()
assert(typeof(op) == "OverlapParams" and op.MaxParts == 0)
op.MaxParts = 5
assert(op.MaxParts == 5)
assert(not pcall(function()
	op.MaxParts = -1
end))
