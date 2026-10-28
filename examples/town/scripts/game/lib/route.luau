--!strict
-- Routes through the town's street grid: the way along the streets to a
-- spot, shortest first (Dijkstra over the junctions, `lib/town`). The
-- streets bend between junctions: each is a centripetal Catmull-Rom spline
-- through its junctions, as scripts/generators/lib/plan.luau draws it, so a
-- route is made of the spline's points about every STEP metres, and a truck
-- that follows it stays on the road.
local town = require("./town")
local util = require("./util")

local route = {}

-- Turning round in the street costs as much as this many metres of driving.
local U_TURN = 80
-- Metres between a route's points.
local STEP = 5
-- Streets `route.prepare` lays out between two waits (see lib/town).
local BATCH = 12

-- One span of a centripetal Catmull-Rom spline (Barry and Goldman's
-- pyramid), from p1 (u = 0) to p2 (u = 1): the generator's.
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

-- The junction on the far side of `a` from `b` along their street, or a
-- point as far beyond `a` at the street's end (the spline's phantom end).
local function beyond(a: number, b: number): vector
	local next_one = town.junctions[2 * a - b]
	if next_one then
		return next_one.at
	end
	return town.junctions[a].at * 2 - town.junctions[b].at
end

-- Street points between neighbouring junctions, and their lengths, by
-- `a * 10000 + b`, for one version of the town.
local cache: { [number]: { vector } } = {}
local lengths: { [number]: number } = {}
local cache_version = -1

-- The street's points from junction `a` to its neighbour `b`, both included,
-- about STEP metres apart.
local function between(a: number, b: number): { vector }
	if cache_version ~= town.version then
		cache = {}
		lengths = {}
		cache_version = town.version
	end
	local key = a * 10000 + b
	local known = cache[key]
	if known then
		return known
	end
	-- The spline runs from the lower id to the higher; sample it that way.
	local lo, hi = math.min(a, b), math.max(a, b)
	local p1, p2 = town.junctions[lo].at, town.junctions[hi].at
	local p0, p3 = beyond(lo, hi), beyond(hi, lo)
	local steps = math.max(1, math.ceil(util.flat_distance(p1, p2) / STEP))
	local points = table.create(steps + 1) :: { vector }
	for k = 0, steps do
		points[k + 1] = span(p0, p1, p2, p3, k / steps)
	end
	if a > b then
		local n = #points
		for i = 1, n // 2 do
			points[i], points[n + 1 - i] = points[n + 1 - i], points[i]
		end
	end
	cache[key] = points
	local sum = 0
	for i = 1, #points - 1 do
		sum += util.flat_distance(points[i], points[i + 1])
	end
	lengths[key] = sum
	return points
end

-- The length of the street from junction `a` to its neighbour `b`.
local function street_length(a: number, b: number): number
	between(a, b)
	return lengths[a * 10000 + b]
end

-- Lays out every street of the town ahead of the first route, waiting for
-- the next step after every BATCH of them: call it after `town.refresh`, from
-- a thread that may wait.
function route.prepare()
	local n = 0
	for id, j in town.junctions do
		for _, other in j.links do
			between(id, other)
			n += 1
			if n % BATCH == 0 then
				task.wait()
			end
		end
	end
end

-- Where a point is along the streets: on the stretch from junction `a` to
-- `b` (a < b), on its `k`-th segment (between points k and k + 1 of
-- `between(a, b)`), `distance` metres from it.
type Place = { a: number, b: number, k: number, distance: number }

-- The stretch of street nearest `p`.
local function place_of(p: vector): Place?
	local best: Place? = nil
	for id, j in town.junctions do
		for _, other in j.links do
			if other > id then
				local points = between(id, other)
				for k = 1, #points - 1 do
					local s, e = points[k], points[k + 1]
					local se = vector.create(e.x - s.x, 0, e.z - s.z)
					local sp = vector.create(p.x - s.x, 0, p.z - s.z)
					local len2 = vector.dot(se, se)
					local t = if len2 > 0 then math.clamp(vector.dot(sp, se) / len2, 0, 1) else 0
					local d = vector.magnitude(sp - se * t)
					if not best or d < best.distance then
						best = { a = id, b = other, k = k, distance = d }
					end
				end
			end
		end
	end
	return best
end

-- The length of a run of points.
local function length(points: { vector }, from: number, to: number): number
	local sum = 0
	for i = from, to - 1 do
		sum += util.flat_distance(points[i], points[i + 1])
	end
	return sum
end

-- The cheapest way from the start costs to every junction. `end_a` and
-- `end_b` end the start's stretch, which the search does not drive along
-- (`start` has the costs of that), so a route never turns round at a
-- junction.
local function search(start: { [number]: number }, end_a: number, end_b: number): ({ [number]: number }, { [number]: number })
	local cost: { [number]: number } = {}
	local via: { [number]: number } = {}
	local open: { number } = {}
	for id, c in start do
		cost[id] = c
		table.insert(open, id)
	end
	while #open > 0 do
		-- The cheapest open junction (ties: lowest id), without a heap: the
		-- grid has a few dozen junctions.
		local best_i = 1
		for i = 2, #open do
			local a, b = open[i], open[best_i]
			if cost[a] < cost[b] or (cost[a] == cost[b] and a < b) then
				best_i = i
			end
		end
		local id = table.remove(open, best_i) :: number
		for _, other in town.junctions[id].links do
			if (id == end_a and other == end_b) or (id == end_b and other == end_a) then
				continue
			end
			local c = cost[id] + street_length(id, other)
			if cost[other] == nil or c < cost[other] then
				if cost[other] == nil then
					table.insert(open, other)
				end
				cost[other] = c
				via[other] = id
			end
		end
	end
	return cost, via
end

-- The points to drive through from `from` (facing `heading`) to `to`: along
-- the street from where the truck is, through the junctions on the way, and
-- along the last street to `to` itself.
function route.plan(from: vector, heading: vector, to: vector): { vector }
	local s = place_of(from)
	local e = place_of(to)
	if not s or not e then
		return { to }
	end
	local here = between(s.a, s.b)
	local n = #here
	-- On the same stretch: along it (turning round in the street first when
	-- `to` is behind).
	if s.a == e.a and s.b == e.b then
		local points: { vector } = {}
		if e.k > s.k or (e.k == s.k and vector.dot(here[s.k + 1] - here[s.k], to - from) > 0) then
			for i = s.k + 1, e.k do
				table.insert(points, here[i])
			end
		else
			for i = s.k, e.k + 1, -1 do
				table.insert(points, here[i])
			end
		end
		table.insert(points, to)
		return points
	end
	-- Starting costs: the drive along the street to each end of the start's
	-- stretch, and a turn when that end is behind.
	local start: { [number]: number } = {
		[s.a] = length(here, 1, s.k + 1),
		[s.b] = length(here, s.k, n),
	}
	local forward = here[s.k + 1] - here[s.k]
	if vector.dot(vector.create(forward.x, 0, forward.z), heading) > 0 then
		start[s.a] += U_TURN
	else
		start[s.b] += U_TURN
	end
	local cost, via = search(start, s.a, s.b)
	local there = between(e.a, e.b)
	local m = #there
	local to_a = cost[e.a] + length(there, 1, e.k + 1)
	local to_b = cost[e.b] + length(there, e.k, m)
	local last = if to_a <= to_b then e.a else e.b
	local ids: { number } = {}
	local id: number? = last
	while id do
		table.insert(ids, 1, id)
		id = via[id]
	end

	local points: { vector } = {}
	-- Along the start's stretch to the first junction.
	if ids[1] == s.b then
		for i = s.k + 1, n do
			table.insert(points, here[i])
		end
	else
		for i = s.k, 1, -1 do
			table.insert(points, here[i])
		end
	end
	-- Junction to junction.
	for i = 1, #ids - 1 do
		local leg = between(ids[i], ids[i + 1])
		for k = 2, #leg do
			table.insert(points, leg[k])
		end
	end
	-- Along the last stretch to `to`.
	if last == e.a then
		for i = 2, e.k do
			table.insert(points, there[i])
		end
	else
		for i = m - 1, e.k + 1, -1 do
			table.insert(points, there[i])
		end
	end
	table.insert(points, to)
	return points
end

-- A random address's spot, by `pick` (a function giving 1 to n).
function route.random_spot(pick: (number) -> number): vector
	local list = town.addresses
	if #list == 0 then
		return town.bay
	end
	return list[pick(#list)].spot
end

return route
