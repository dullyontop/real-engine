--!strict
-- Helpers the town's node kinds share: the points records (ADR-0043 §4:
-- 28 bytes each, u32 id then f32 position and normal), the junction ids,
-- and the curve every street follows, so that `project.street`, which
-- draws a street, and `project.town`, which builds along it, agree on
-- where it runs.
local plan = {}

plan.POINT_BYTES = 28

-- Block points' ids start here (see project.town_plan).
plan.BLOCK = 10000

export type Point = { id: number, position: vector, normal: vector }

-- A junction's id from its row (along Z) and column (along X).
function plan.id(row: number, col: number): number
	return row * 100 + col
end

function plan.row(id: number): number
	return id // 100
end

function plan.col(id: number): number
	return id % 100
end

function plan.write_point(data: buffer, index: number, id: number, position: vector, normal: vector?)
	local n = normal or vector.create(0, 1, 0)
	local at = index * plan.POINT_BYTES
	buffer.writeu32(data, at, id)
	buffer.writef32(data, at + 4, position.x)
	buffer.writef32(data, at + 8, position.y)
	buffer.writef32(data, at + 12, position.z)
	buffer.writef32(data, at + 16, n.x)
	buffer.writef32(data, at + 20, n.y)
	buffer.writef32(data, at + 24, n.z)
end

-- The points of a points value, in order.
function plan.read_points(points: { count: number, data: buffer }): { Point }
	local out = table.create(points.count) :: { Point }
	for i = 0, points.count - 1 do
		local at = i * plan.POINT_BYTES
		out[i + 1] = {
			id = buffer.readu32(points.data, at),
			position = vector.create(
				buffer.readf32(points.data, at + 4),
				buffer.readf32(points.data, at + 8),
				buffer.readf32(points.data, at + 12)
			),
			normal = vector.create(
				buffer.readf32(points.data, at + 16),
				buffer.readf32(points.data, at + 20),
				buffer.readf32(points.data, at + 24)
			),
		}
	end
	return out
end

export type Plan = {
	rows: number,
	cols: number,
	-- Junction positions by id.
	at: { [number]: vector },
}

-- The street plan in a `project.town_plan` output.
function plan.read(points: { count: number, data: buffer }): Plan
	local at = {}
	local rows, cols = 0, 0
	for _, p in plan.read_points(points) do
		if p.id < plan.BLOCK then
			at[p.id] = p.position
			rows = math.max(rows, plan.row(p.id) + 1)
			cols = math.max(cols, plan.col(p.id) + 1)
		end
	end
	return { rows = rows, cols = cols, at = at }
end

export type Block = {
	-- The row and column of its north-west junction.
	row: number,
	col: number,
	-- `houses`, `park` or `depot`.
	use: string,
	-- Its middle, at the road's height.
	middle: vector,
}

-- The blocks in a `project.town_plan` output, row by row.
function plan.blocks(points: { count: number, data: buffer, tags: { string }? }): { Block }
	local out: { Block } = {}
	local tags = points.tags or {}
	for i, p in plan.read_points(points) do
		if p.id >= plan.BLOCK then
			local id = p.id - plan.BLOCK
			local use: string = tags[i] or "houses"
			table.insert(out, { row = plan.row(id), col = plan.col(id), use = use, middle = p.position })
		end
	end
	return out
end

export type Street = {
	-- "row" (runs along X, east-west) or "col" (runs along Z, north-south).
	axis: string,
	index: number,
	-- On the town's edge.
	ring: boolean,
	-- The middle row or column: the main streets.
	main: boolean,
	-- Its junctions, in order.
	junctions: { vector },
}

-- Every street of the plan: rows first, then columns.
function plan.streets(p: Plan): { Street }
	local out = {}
	for r = 0, p.rows - 1 do
		local js = {}
		for c = 0, p.cols - 1 do
			table.insert(js, p.at[plan.id(r, c)])
		end
		table.insert(out, {
			axis = "row",
			index = r,
			ring = r == 0 or r == p.rows - 1,
			main = r == p.rows // 2,
			junctions = js,
		})
	end
	for c = 0, p.cols - 1 do
		local js = {}
		for r = 0, p.rows - 1 do
			table.insert(js, p.at[plan.id(r, c)])
		end
		table.insert(out, {
			axis = "col",
			index = c,
			ring = c == 0 or c == p.cols - 1,
			main = c == p.cols // 2,
			junctions = js,
		})
	end
	return out
end

-- A street's width in metres.
function plan.width(s: Street): number
	if s.main then
		return 10
	elseif s.ring then
		return 9
	end
	return 7.5
end

-- One span of a centripetal Catmull-Rom spline (Barry and Goldman's
-- pyramid), from p1 (u = 0) to p2 (u = 1).
local function span(p0: vector, p1: vector, p2: vector, p3: vector, u: number): vector
	local t0 = 0
	local t1 = t0 + math.sqrt(vector.magnitude(p1 - p0))
	local t2 = t1 + math.sqrt(vector.magnitude(p2 - p1))
	local t3 = t2 + math.sqrt(vector.magnitude(p3 - p2))
	local t = t1 + (t2 - t1) * u
	local a1 = p0 * ((t1 - t) / (t1 - t0)) + p1 * ((t - t0) / (t1 - t0))
	local a2 = p1 * ((t2 - t) / (t2 - t1)) + p2 * ((t - t1) / (t2 - t1))
	local a3 = p2 * ((t3 - t) / (t3 - t2)) + p3 * ((t - t2) / (t3 - t2))
	local b1 = a1 * ((t2 - t) / (t2 - t0)) + a2 * ((t - t0) / (t2 - t0))
	local b2 = a2 * ((t3 - t) / (t3 - t1)) + a3 * ((t - t1) / (t3 - t1))
	return b1 * ((t2 - t) / (t2 - t1)) + b2 * ((t - t1) / (t2 - t1))
end

-- The street's centreline: the spline through its junctions, sampled about
-- every `step` metres, carried on straight for `extend` metres past each end
-- (so that the streets on the edge meet in square corners and the others
-- cross the edge streets).
function plan.centreline(s: Street, step: number, extend: number): { vector }
	local js = s.junctions
	local n = #js
	local out: { vector } = {}
	local first_dir = vector.normalize(js[2] - js[1])
	local last_dir = vector.normalize(js[n] - js[n - 1])
	if extend > 0 then
		table.insert(out, js[1] - first_dir * extend)
	end
	for i = 1, n - 1 do
		local p0 = if i > 1 then js[i - 1] else js[1] * 2 - js[2]
		local p3 = if i + 2 <= n then js[i + 2] else js[n] * 2 - js[n - 1]
		local p1, p2 = js[i], js[i + 1]
		local steps = math.max(1, math.ceil(vector.magnitude(p2 - p1) / step))
		for k = 0, steps - 1 do
			table.insert(out, span(p0, p1, p2, p3, k / steps))
		end
	end
	table.insert(out, js[n])
	if extend > 0 then
		table.insert(out, js[n] + last_dir * extend)
	end
	return out
end

-- The distance from `p` to the polyline `line` in X and Z, and the
-- polyline's direction there.
function plan.nearest(line: { vector }, p: vector): (number, vector)
	local best, dir = math.huge, vector.create(1, 0, 0)
	for i = 1, #line - 1 do
		local a, b = line[i], line[i + 1]
		local ab = vector.create(b.x - a.x, 0, b.z - a.z)
		local ap = vector.create(p.x - a.x, 0, p.z - a.z)
		local len2 = vector.dot(ab, ab)
		local t = if len2 > 0 then math.clamp(vector.dot(ap, ab) / len2, 0, 1) else 0
		local d = vector.magnitude(ap - ab * t)
		if d < best then
			best, dir = d, if len2 > 0 then ab / math.sqrt(len2) else dir
		end
	end
	return best, dir
end

-- An sRGB colour (0 to 255) as the linear colour vertices carry (`math.pow`,
-- not `^`, which gives other bits on other platforms: RE3083).
function plan.rgb(r: number, g: number, b: number): vector
	return vector.create(math.pow(r / 255, 2.2), math.pow(g / 255, 2.2), math.pow(b / 255, 2.2))
end

return plan
