local function near(a: number, b: number, eps: number?)
	return math.abs(a - b) <= (eps or 1e-5)
end
local function vnear(a: Vector3, b: Vector3, eps: number?)
	return (a - b).Magnitude <= (eps or 1e-5)
end

local id = CFrame.new()
assert(typeof(id) == "CFrame")
assert(id == CFrame.identity)
assert(CFrame.new(1, 2, 3).Position == Vector3.new(1, 2, 3))
assert(CFrame.new(Vector3.new(1, 2, 3)) == CFrame.new(1, 2, 3))
assert(CFrame.new(1, 2, 3).p == Vector3.new(1, 2, 3))
assert(CFrame.new(1, 2, 3).X == 1 and CFrame.new(1, 2, 3).Z == 3)
assert(id.LookVector == Vector3.new(0, 0, -1))
assert(id.RightVector == Vector3.xAxis and id.UpVector == Vector3.yAxis)
assert(id.ZVector == Vector3.zAxis)

-- The documented conventions.
assert(vnear(CFrame.Angles(0, 0, math.rad(90)) * Vector3.xAxis, Vector3.yAxis))
assert(vnear(CFrame.lookAt(Vector3.zero, Vector3.new(0, 0, -1)).LookVector, Vector3.new(0, 0, -1)))
local a, b, c = 0.3, -1.1, 2.0
local composed = CFrame.Angles(a, 0, 0) * CFrame.Angles(0, b, 0) * CFrame.Angles(0, 0, c)
assert(CFrame.Angles(a, b, c):FuzzyEq(composed, 1e-6))
assert(CFrame.fromEulerAnglesXYZ(a, b, c) == CFrame.Angles(a, b, c))
local yxz = CFrame.Angles(0, b, 0) * CFrame.Angles(a, 0, 0) * CFrame.Angles(0, 0, c)
assert(CFrame.fromEulerAnglesYXZ(a, b, c):FuzzyEq(yxz, 1e-6))
assert(CFrame.fromOrientation(a, b, c) == CFrame.fromEulerAnglesYXZ(a, b, c))

-- Round trips.
local cf = CFrame.new(5, -2, 7) * CFrame.Angles(a, b, c)
local x, y, z = cf:ToEulerAnglesXYZ()
assert(near(x, a) and near(y, b) and near(z, c))
x, y, z = CFrame.fromEulerAnglesYXZ(a, b, c):ToEulerAnglesYXZ()
assert(near(x, a) and near(y, b) and near(z, c))
x, y, z = CFrame.fromOrientation(a, b, c):ToOrientation()
assert(near(x, a) and near(y, b) and near(z, c))
assert((cf * cf:Inverse()):FuzzyEq(CFrame.identity))
local p = Vector3.new(1, 2, 3)
assert(vnear(cf:PointToObjectSpace(cf:PointToWorldSpace(p)), p))
assert(vnear(cf:VectorToObjectSpace(cf:VectorToWorldSpace(p)), p))
assert(vnear(cf:PointToWorldSpace(p), cf * p))
local other = CFrame.new(1, 1, 1) * CFrame.Angles(1, 0, 0)
assert(cf:ToWorldSpace(cf:ToObjectSpace(other)):FuzzyEq(other))
assert(cf:ToObjectSpace(other):FuzzyEq(cf:Inverse() * other))
-- The space methods take several arguments.
local p1, p2 = cf:PointToWorldSpace(Vector3.zero, Vector3.one)
assert(p1 == cf.Position and vnear(p2, cf * Vector3.one))

-- Axis and angle.
local axis, angle = CFrame.fromAxisAngle(Vector3.new(0, 2, 0), 1.25):ToAxisAngle()
assert(vnear(axis, Vector3.yAxis) and near(angle, 1.25))
axis, angle = CFrame.identity:ToAxisAngle()
assert(axis == Vector3.xAxis and angle == 0)

-- Operators.
assert((CFrame.new(1, 2, 3) + Vector3.new(1, 1, 1)).Position == Vector3.new(2, 3, 4))
assert((CFrame.new(1, 2, 3) - Vector3.new(1, 1, 1)).Position == Vector3.new(0, 1, 2))
assert((CFrame.Angles(0, 1, 0) + Vector3.one).Rotation == CFrame.Angles(0, 1, 0))

-- Components.
local comps = { CFrame.new(1, 2, 3):GetComponents() }
assert(#comps == 12 and comps[1] == 1 and comps[4] == 1 and comps[5] == 0)
assert(CFrame.new(table.unpack(comps)) == CFrame.new(1, 2, 3))
assert(CFrame.new(cf:GetComponents()) == cf)
assert(CFrame.new(1, 2, 3).R00 == 1 and CFrame.new(1, 2, 3).R01 == 0)

-- Quaternion form: 90 degrees about Y.
local s = math.sqrt(0.5)
assert(CFrame.new(0, 0, 0, 0, s, 0, s):FuzzyEq(CFrame.Angles(0, math.pi / 2, 0)))

-- Lerp ends.
local goal = CFrame.new(10, 0, 0) * CFrame.Angles(0, math.pi / 2, 0)
assert(id:Lerp(goal, 0) == id)
assert(id:Lerp(goal, 1):FuzzyEq(goal))
assert(id:Lerp(goal, 0.5):FuzzyEq(CFrame.new(5, 0, 0) * CFrame.Angles(0, math.pi / 4, 0)))

-- The deprecated two-vector form looks at the point.
assert(vnear(CFrame.new(Vector3.zero, Vector3.new(10, 0, 0)).LookVector, Vector3.xAxis))
assert(vnear(CFrame.lookAt(Vector3.zero, Vector3.new(0, 10, 0)).LookVector, Vector3.yAxis))

assert(tostring(CFrame.new(1, 2, 3)) == "1, 2, 3, 1, 0, 0, 0, 1, 0, 0, 0, 1")
local ok, err = pcall(function()
	return CFrame.new(1, 2)
end)
assert(not ok and string.find(tostring(err), "RE3006", 1, true))
ok, err = pcall(function()
	return (cf :: any).Bogus
end)
assert(not ok and string.find(tostring(err), "Bogus is not a valid member of CFrame", 1, true))
ok = pcall(function()
	return cf * 2
end)
assert(not ok)
