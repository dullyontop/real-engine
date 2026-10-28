--!strict
-- A small mesh builder for the town's node kinds: flat-shaded quads and
-- triangles with a colour per face, in a local frame placed by `origin` and
-- `yaw`, written out as the buffers a mesh value carries (ADR-0043 §4).
-- Faces wind counter-clockwise seen from the front, the side the renderer
-- draws.
local meshb = {}

export type Builder = {
	positions: { number },
	normals: { number },
	colors: { number },
	indices: { number },
	count: number,
	-- The frame faces are given in: a world position and a turn about +Y.
	origin: vector,
	cos: number,
	sin: number,
}

function meshb.new(): Builder
	return {
		positions = {},
		normals = {},
		colors = {},
		indices = {},
		count = 0,
		origin = vector.zero,
		cos = 1,
		sin = 0,
	}
end

-- Places the faces added next: local +Z turns to (sin yaw, 0, cos yaw).
function meshb.frame(b: Builder, origin: vector, yaw: number)
	b.origin = origin
	b.cos = math.cos(yaw)
	b.sin = math.sin(yaw)
end

-- A local vector turned into the frame (without the origin).
function meshb.turn(b: Builder, v: vector): vector
	return vector.create(v.x * b.cos + v.z * b.sin, v.y, -v.x * b.sin + v.z * b.cos)
end

-- A local point in the frame.
function meshb.place(b: Builder, v: vector): vector
	return b.origin + meshb.turn(b, v)
end

local function push(b: Builder, p: vector, n: vector, c: vector)
	local pos, nor, col = b.positions, b.normals, b.colors
	table.insert(pos, p.x)
	table.insert(pos, p.y)
	table.insert(pos, p.z)
	table.insert(nor, n.x)
	table.insert(nor, n.y)
	table.insert(nor, n.z)
	table.insert(col, c.x)
	table.insert(col, c.y)
	table.insert(col, c.z)
	table.insert(col, 1)
	b.count += 1
end

-- A polygon of 3 or 4 local points, counter-clockwise from the front.
function meshb.face(b: Builder, color: vector, a: vector, c: vector, d: vector, e: vector?)
	local pa, pc, pd = meshb.place(b, a), meshb.place(b, c), meshb.place(b, d)
	local n = vector.normalize(vector.cross(pc - pa, pd - pa))
	local base = b.count
	push(b, pa, n, color)
	push(b, pc, n, color)
	push(b, pd, n, color)
	local idx = b.indices
	table.insert(idx, base)
	table.insert(idx, base + 1)
	table.insert(idx, base + 2)
	if e then
		push(b, meshb.place(b, e), n, color)
		table.insert(idx, base)
		table.insert(idx, base + 2)
		table.insert(idx, base + 3)
	end
end

-- A box from `lo` to `hi` (local corners); `bottom` adds its underside.
function meshb.box(b: Builder, color: vector, lo: vector, hi: vector, bottom: boolean?)
	local x0, y0, z0, x1, y1, z1 = lo.x, lo.y, lo.z, hi.x, hi.y, hi.z
	local v = vector.create
	meshb.face(b, color, v(x0, y0, z1), v(x1, y0, z1), v(x1, y1, z1), v(x0, y1, z1)) -- front (+Z)
	meshb.face(b, color, v(x1, y0, z0), v(x0, y0, z0), v(x0, y1, z0), v(x1, y1, z0)) -- back
	meshb.face(b, color, v(x1, y0, z1), v(x1, y0, z0), v(x1, y1, z0), v(x1, y1, z1)) -- right (+X)
	meshb.face(b, color, v(x0, y0, z0), v(x0, y0, z1), v(x0, y1, z1), v(x0, y1, z0)) -- left
	meshb.face(b, color, v(x0, y1, z1), v(x1, y1, z1), v(x1, y1, z0), v(x0, y1, z0)) -- top
	if bottom then
		meshb.face(b, color, v(x0, y0, z0), v(x1, y0, z0), v(x1, y0, z1), v(x0, y0, z1))
	end
end

-- A flat panel on the front (+Z) of a wall at depth `z`, from (x0, y0) to (x1, y1).
function meshb.panel(b: Builder, color: vector, x0: number, y0: number, x1: number, y1: number, z: number)
	local v = vector.create
	meshb.face(b, color, v(x0, y0, z), v(x1, y0, z), v(x1, y1, z), v(x0, y1, z))
end

local function f32s(values: { number }): buffer
	local out = buffer.create(#values * 4)
	for i, x in values do
		buffer.writef32(out, (i - 1) * 4, x)
	end
	return out
end

local function u32s(values: { number }): buffer
	local out = buffer.create(#values * 4)
	for i, x in values do
		buffer.writeu32(out, (i - 1) * 4, x)
	end
	return out
end

-- The mesh value, drawn with `material`.
function meshb.finish(b: Builder, material: string)
	return {
		positions = f32s(b.positions),
		normals = f32s(b.normals),
		colors = f32s(b.colors),
		indices = u32s(b.indices),
		material = material,
	}
end

return meshb
