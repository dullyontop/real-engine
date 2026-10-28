--!strict
-- Polygon helpers for the town's node kinds, in X and Z: a ring is a list
-- of `{x, z}` points (a shapes value's `outer`).
local poly = {}

export type Ring = { { number } }

-- Twice the signed area: positive when the ring runs counter-clockwise in
-- (x, z), that is clockwise seen from above.
function poly.area2(ring: Ring): number
	local sum = 0
	local n = #ring
	for i = 1, n do
		local a, b = ring[i], ring[i % n + 1]
		sum += a[1] * b[2] - b[1] * a[2]
	end
	return sum
end

-- Whether `p` (x, z) is inside the ring (even-odd).
function poly.contains(ring: Ring, x: number, z: number): boolean
	local inside = false
	local n = #ring
	local j = n
	for i = 1, n do
		local a, b = ring[i], ring[j]
		if (a[2] > z) ~= (b[2] > z) and x < (b[1] - a[1]) * (z - a[2]) / (b[2] - a[2]) + a[1] then
			inside = not inside
		end
		j = i
	end
	return inside
end

export type Side = {
	-- The side's points, in the ring's order.
	points: { vector },
	-- The length along them.
	length: number,
	-- The direction from its first point to its last.
	dir: vector,
	-- The unit normal pointing into the ring.
	inward: vector,
}

-- The ring's sides: runs of edges that turn less than `max_turn` radians
-- from the direction the run started in, longest first in ties of order.
-- Points are at height `y`.
function poly.sides(ring: Ring, y: number, max_turn: number): { Side }
	local n = #ring
	local ccw = poly.area2(ring) > 0
	local pts = table.create(n) :: { vector }
	for i, p in ring do
		pts[i] = vector.create(p[1], y, p[2])
	end
	-- Start at a corner, so that no side wraps around the ring's start.
	local cos_turn = math.cos(max_turn)
	local function dir_at(i: number): vector
		local a, b = pts[(i - 1) % n + 1], pts[i % n + 1]
		local d = b - a
		local len = vector.magnitude(d)
		return if len > 0 then d / len else vector.zero
	end
	local start = 1
	for i = 1, n do
		if vector.dot(dir_at(i - 1 + n), dir_at(i)) < cos_turn then
			start = i
			break
		end
	end
	local out: { Side } = {}
	local current: { vector } = { pts[start] }
	local base = dir_at(start)
	for k = 0, n - 1 do
		local i = (start - 1 + k) % n + 1
		local d = dir_at(i)
		if k > 0 and vector.dot(base, d) < cos_turn then
			table.insert(out, current :: any)
			current = { pts[i] }
			base = d
		end
		table.insert(current, pts[i % n + 1])
	end
	table.insert(out, current :: any)
	local sides: { Side } = {}
	for _, points in out :: { { vector } } do
		local length = 0
		for i = 1, #points - 1 do
			length += vector.magnitude(points[i + 1] - points[i])
		end
		local chord = points[#points] - points[1]
		local clen = vector.magnitude(chord)
		if clen > 1e-6 then
			local dir = chord / clen
			-- Left of the direction in (x, z) is (-dz, dx).
			local left = vector.create(-dir.z, 0, dir.x)
			table.insert(sides, {
				points = points,
				length = length,
				dir = dir,
				inward = if ccw then left else -left,
			})
		end
	end
	return sides
end

-- The point `s` metres along a polyline, and the direction there.
function poly.along(points: { vector }, s: number): (vector, vector)
	local walked = 0
	for i = 1, #points - 1 do
		local a, b = points[i], points[i + 1]
		local len = vector.magnitude(b - a)
		if len > 0 and (walked + len >= s or i == #points - 1) then
			local t = math.clamp((s - walked) / len, 0, 1)
			return a + (b - a) * t, (b - a) / len
		end
		walked += len
	end
	return points[1], vector.create(1, 0, 0)
end

export type Box = { centre: vector, axis: vector, half_w: number, half_d: number }

-- Whether two rectangles in X and Z overlap (separating axes). `axis` is
-- each box's unit width direction; its depth runs across it.
function poly.overlap(a: Box, b: Box): boolean
	local function across(v: vector): vector
		return vector.create(-v.z, 0, v.x)
	end
	local d = b.centre - a.centre
	for _, axis in { a.axis, across(a.axis), b.axis, across(b.axis) } do
		local ra = a.half_w * math.abs(vector.dot(a.axis, axis)) + a.half_d * math.abs(vector.dot(across(a.axis), axis))
		local rb = b.half_w * math.abs(vector.dot(b.axis, axis)) + b.half_d * math.abs(vector.dot(across(b.axis), axis))
		if math.abs(vector.dot(d, axis)) > ra + rb then
			return false
		end
	end
	return true
end

-- The four corners of a box.
function poly.corners(b: Box): { vector }
	local across = vector.create(-b.axis.z, 0, b.axis.x)
	local w, d = b.axis * b.half_w, across * b.half_d
	return { b.centre - w - d, b.centre + w - d, b.centre + w + d, b.centre - w + d }
end

return poly
