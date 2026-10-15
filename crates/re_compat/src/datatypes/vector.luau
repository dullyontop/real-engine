--!strict
-- The Vector3 library and the vector type metatable (re_compat, WP10).
-- Vector3 is Luau's native vector: arithmetic, ==, .X/.Y/.Z and .x/.y/.z are
-- builtin; the members below are added through the type metatable. The math
-- uses Luau's vector library, which works in f32 as Roblox does.
local tostr = ...

local create = vector.create
local magnitude = vector.magnitude
local normalize = vector.normalize
local dot = vector.dot
local cross = vector.cross
local abs = vector.abs
local floor = vector.floor
local ceil = vector.ceil
local sign = vector.sign
local vmin = vector.min
local vmax = vector.max

local function fail(msg: string): never
	error(msg, 3)
end

local function check(v: any, n: number, fname: string): vector
	if type(v) ~= "vector" then
		local t = typeof(v)
		if t == "vector" then
			t = "Vector3"
		end
		fail(`[RE3006] invalid argument #{n} to '{fname}' (Vector3 expected, got {t})`)
	end
	return v
end

local function checknum(x: any, n: number, fname: string): number
	local v = tonumber(x)
	if v == nil then
		fail(`[RE3006] invalid argument #{n} to '{fname}' (number expected, got {typeof(x)})`)
	end
	return v :: number
end

local methods = {}

function methods.Dot(a: vector, b: vector): number
	return dot(a, check(b, 1, "Dot"))
end

function methods.Cross(a: vector, b: vector): vector
	return cross(a, check(b, 1, "Cross"))
end

function methods.Lerp(a: vector, b: vector, t: number): vector
	check(b, 1, "Lerp")
	return a + (b - a) * checknum(t, 2, "Lerp")
end
methods.lerp = methods.Lerp

function methods.FuzzyEq(a: vector, b: vector, eps: number?): boolean
	check(b, 1, "FuzzyEq")
	local e = if eps == nil then 1e-5 else checknum(eps, 2, "FuzzyEq")
	local d = abs(a - b)
	return d.X <= e and d.Y <= e and d.Z <= e
end

function methods.Abs(a: vector): vector
	return abs(a)
end

function methods.Floor(a: vector): vector
	return floor(a)
end

function methods.Ceil(a: vector): vector
	return ceil(a)
end

function methods.Sign(a: vector): vector
	return sign(a)
end

function methods.Min(a: vector, ...: vector): vector
	local r = a
	for i = 1, select("#", ...) do
		r = vmin(r, check((select(i, ...)), i, "Min"))
	end
	return r
end

function methods.Max(a: vector, ...: vector): vector
	local r = a
	for i = 1, select("#", ...) do
		r = vmax(r, check((select(i, ...)), i, "Max"))
	end
	return r
end

-- The angle in radians between a and b. With an axis, the angle is signed:
-- positive when a turns to b counter-clockwise looking down the axis.
function methods.Angle(a: vector, b: vector, axis: vector?): number
	check(b, 1, "Angle")
	local angle = math.atan2(magnitude(cross(a, b)), dot(a, b))
	if axis ~= nil then
		check(axis, 2, "Angle")
		if dot(cross(a, b), axis) < 0 then
			angle = -angle
		end
	end
	return angle
end

local mt = {}
mt.__type = "Vector3"

function mt.__index(v: vector, k: any): any
	local m = methods[k]
	if m ~= nil then
		return m
	end
	if k == "Magnitude" or k == "magnitude" then
		return magnitude(v)
	elseif k == "Unit" or k == "unit" then
		return normalize(v)
	end
	return fail(`[RE9001] {tostring(k)} is not a valid member of Vector3`)
end

function mt.__newindex(_v: vector, k: any, _x: any)
	fail(`[RE9001] {tostring(k)} cannot be assigned to: Vector3 values are immutable`)
end

function mt.__tostring(v: vector): string
	return tostr(v)
end

mt.__metatable = "The metatable is locked"
table.freeze(methods)
table.freeze(mt)

local normals = {
	Right = create(1, 0, 0),
	Top = create(0, 1, 0),
	Back = create(0, 0, 1),
	Left = create(-1, 0, 0),
	Bottom = create(0, -1, 0),
	Front = create(0, 0, -1),
}
local axes = { X = create(1, 0, 0), Y = create(0, 1, 0), Z = create(0, 0, 1) }

local function enumName(item: any, enumType: string, n: number, fname: string): string
	if typeof(item) == "EnumItem" and tostring(item.EnumType) == enumType then
		return item.Name
	end
	if type(item) == "string" then
		return item
	end
	return fail(`[RE3006] invalid argument #{n} to '{fname}' (Enum.{enumType} expected, got {typeof(item)})`)
end

local Vector3 = {
	zero = create(0, 0, 0),
	one = create(1, 1, 1),
	xAxis = create(1, 0, 0),
	yAxis = create(0, 1, 0),
	zAxis = create(0, 0, 1),
}

function Vector3.new(x: number?, y: number?, z: number?): vector
	return create(
		if x == nil then 0 else checknum(x, 1, "new"),
		if y == nil then 0 else checknum(y, 2, "new"),
		if z == nil then 0 else checknum(z, 3, "new")
	)
end

function Vector3.FromNormalId(id: any): vector
	local v = normals[enumName(id, "NormalId", 1, "FromNormalId")]
	if v == nil then
		fail(`[RE3006] invalid argument #1 to 'FromNormalId' (not a NormalId: {tostring(id)})`)
	end
	return v
end

function Vector3.FromAxis(axis: any): vector
	local v = axes[enumName(axis, "Axis", 1, "FromAxis")]
	if v == nil then
		fail(`[RE3006] invalid argument #1 to 'FromAxis' (not an Axis: {tostring(axis)})`)
	end
	return v
end

table.freeze(Vector3)

return Vector3, mt
