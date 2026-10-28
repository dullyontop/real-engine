--!strict
-- Where the town's trees, bushes and street lamps go (a Luau node kind,
-- ADR-0043 §4), as tagged points for `points.instance`:
--
-- * `tree` and `bush` in the parks, clear of the fountain and its benches
--   in the middle, and `tree` in the back gardens: the part of each
--   `houses` block deeper than the houses reach.
-- * `lamp` along every street, on alternate sides, at the kerb, kept out of
--   the junctions.
local plan = require("lib/plan")
local poly = require("lib/poly")

-- The distance from (x, z) to the ring's nearest edge.
local function edge_distance(ring: poly.Ring, x: number, z: number): number
	local best = math.huge
	local n = #ring
	for i = 1, n do
		local a, b = ring[i], ring[i % n + 1]
		local abx, abz = b[1] - a[1], b[2] - a[2]
		local apx, apz = x - a[1], z - a[2]
		local len2 = abx * abx + abz * abz
		local t = if len2 > 0 then math.clamp((apx * abx + apz * abz) / len2, 0, 1) else 0
		local dx, dz = apx - abx * t, apz - abz * t
		best = math.min(best, math.sqrt(dx * dx + dz * dz))
	end
	return best
end

return proc.node({
	kind = "project.greenery",
	version = 1,
	doc = "Trees and bushes in the parks and gardens, and lamps along the streets, as tagged points.",
	inputs = {
		{ name = "plan", type = "points", required = true, doc = "The junctions and blocks (project.town_plan)." },
		{ name = "blocks", type = "shapes", required = true, doc = "The blocks: the town minus its roads and pavements." },
	},
	output = "points",
	uses_seed = true,
	params = {
		ground = proc.param.number({ default = 6.0, doc = "The town's ground height, in metres." }),
		spacing = proc.param.number({ default = 7, min = 3, max = 30, doc = "The least distance between two trees, in metres." }),
		garden = proc.param.number({ default = 17, min = 0, max = 40, doc = "How far from a block's edge back-garden trees start, in metres (past the houses)." }),
		lamps = proc.param.number({ default = 30, min = 10, max = 200, doc = "Metres between lamps along a street." }),
	},
	eval = function(ctx, inputs, params)
		local rng = ctx.rng
		local out: { { at: vector, tag: string } } = {}
		local p = plan.read(inputs.plan)
		local shapes = inputs.blocks :: { { outer: poly.Ring } }

		-- Dart throwing: candidates in the ring's box, kept when inside the
		-- ring, `edge` metres from its edge, and `spacing` from every tree.
		local function scatter(ring: poly.Ring, edge: number, keep: (vector) -> boolean, tag: () -> string)
			local x0, z0, x1, z1 = math.huge, math.huge, -math.huge, -math.huge
			for _, q in ring do
				x0 = math.min(x0, q[1])
				x1 = math.max(x1, q[1])
				z0 = math.min(z0, q[2])
				z1 = math.max(z1, q[2])
			end
			local mine: { vector } = {}
			local tries = math.ceil((x1 - x0) * (z1 - z0) / (params.spacing * params.spacing)) * 6
			for _ = 1, tries do
				local at = vector.create(x0 + rng:next() * (x1 - x0), params.ground, z0 + rng:next() * (z1 - z0))
				if poly.contains(ring, at.x, at.z) and edge_distance(ring, at.x, at.z) >= edge and keep(at) then
					local free = true
					for _, other in mine do
						if vector.magnitude(other - at) < params.spacing then
							free = false
							break
						end
					end
					if free then
						table.insert(mine, at)
						table.insert(out, { at = at, tag = tag() })
					end
				end
			end
		end

		for _, block in plan.blocks(inputs.plan) do
			local ring: poly.Ring? = nil
			for _, shape in shapes do
				if poly.contains(shape.outer, block.middle.x, block.middle.z) then
					ring = shape.outer
					break
				end
			end
			if not ring then
				continue
			end
			local middle = vector.create(block.middle.x, params.ground, block.middle.z)
			if block.use == "park" then
				scatter(ring, 2.5, function(at)
					return vector.magnitude(at - middle) > 9
				end, function()
					return if rng:next() < 0.7 then "tree" else "bush"
				end)
			elseif block.use == "houses" then
				scatter(ring, params.garden, function()
					return true
				end, function()
					return "tree"
				end)
			end
		end

		-- Lamps, on alternate sides of each street, at the kerb.
		for _, s in plan.streets(p) do
			local line = plan.centreline(s, 2, 0)
			local half = plan.width(s) / 2 + 0.7
			local walked, next_at, side = 0, params.lamps / 2, 1
			for i = 1, #line - 1 do
				local a, b = line[i], line[i + 1]
				local len = vector.magnitude(b - a)
				while len > 0 and walked + len >= next_at do
					local t = (next_at - walked) / len
					local at = a + (b - a) * t
					local dir = (b - a) / len
					local left = vector.create(-dir.z, 0, dir.x)
					local spot = at + left * (half * side)
					local clear = true
					for _, j in p.at do
						if vector.magnitude(vector.create(j.x - spot.x, 0, j.z - spot.z)) < 12 then
							clear = false
						end
					end
					if clear then
						table.insert(out, { at = vector.create(spot.x, params.ground, spot.z), tag = "lamp" })
					end
					next_at += params.lamps
					side = -side
				end
				walked += len
			end
		end

		local data = buffer.create(#out * plan.POINT_BYTES)
		local tags = table.create(#out)
		for i, q in out do
			plan.write_point(data, i - 1, i, q.at)
			tags[i] = q.tag
		end
		return { count = #out, data = data, tags = tags }
	end,
})
