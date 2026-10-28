--!strict
-- The town's street plan (a Luau node kind, ADR-0043 §4): a grid of
-- junctions, `rows` by `cols`, `spacing` metres apart and centred on the
-- source, and what each block between them is for. The ring of junctions on
-- the edge stays on the grid, so the outer streets are straight; the
-- junctions inside it move by up to `jitter` of the spacing, so each seed
-- bends the inner streets differently.
--
-- Junctions are points tagged `junction` with the id `row * 100 + col`, at
-- the road's height. Each block is a point at its middle tagged with its use
-- (`houses`, `park` or `depot`) with the id `10000 + row * 100 + col` of its
-- north-west junction. The other node kinds read the plan with lib/plan.
local plan = require("lib/plan")

return proc.node({
	kind = "project.town_plan",
	version = 1,
	doc = "The junctions of the town's street grid and the use of each block, as tagged points.",
	output = "points",
	uses_seed = true,
	params = {
		rows = proc.param.integer({ default = 5, min = 3, max = 9, doc = "Junctions along Z (streets running east-west)." }),
		cols = proc.param.integer({ default = 5, min = 3, max = 9, doc = "Junctions along X (avenues running north-south)." }),
		spacing = proc.param.number({ default = 64, min = 40, max = 200, doc = "Metres between neighbouring junctions." }),
		jitter = proc.param.number({ default = 0.16, min = 0, max = 0.25, doc = "How far an inner junction may move, as a fraction of the spacing." }),
		level = proc.param.number({ default = 6.05, doc = "The road surface's height, in metres." }),
		parks = proc.param.integer({ default = 2, min = 0, max = 4, doc = "How many blocks are parks." }),
	},
	eval = function(ctx, _inputs, params)
		local rows, cols, spacing = params.rows, params.cols, params.spacing
		local points: { { id: number, at: vector, tag: string } } = {}
		local at: { [number]: vector } = {}
		for r = 0, rows - 1 do
			for c = 0, cols - 1 do
				local x = (c - (cols - 1) / 2) * spacing
				local z = (r - (rows - 1) / 2) * spacing
				if r > 0 and r < rows - 1 and c > 0 and c < cols - 1 then
					x += (ctx.rng:next() * 2 - 1) * params.jitter * spacing
					z += (ctx.rng:next() * 2 - 1) * params.jitter * spacing
				end
				local p = vector.create(x, params.level, z)
				at[plan.id(r, c)] = p
				table.insert(points, { id = plan.id(r, c), at = p, tag = "junction" })
			end
		end

		-- The depot is on the town's edge, so the truck reaches open road
		-- quickly; the parks are anywhere else.
		local blocks: { { r: number, c: number, use: string } } = {}
		local edge: { number } = {}
		for r = 0, rows - 2 do
			for c = 0, cols - 2 do
				table.insert(blocks, { r = r, c = c, use = "houses" })
				if r == 0 or c == 0 or r == rows - 2 or c == cols - 2 then
					table.insert(edge, #blocks)
				end
			end
		end
		local depot = edge[ctx.rng:int(1, #edge)]
		blocks[depot].use = "depot"
		local placed = 0
		while placed < math.min(params.parks, #blocks - 1) do
			local b = blocks[ctx.rng:int(1, #blocks)]
			if b.use == "houses" then
				b.use = "park"
				placed += 1
			end
		end
		for _, b in blocks do
			local corners = at[plan.id(b.r, b.c)]
				+ at[plan.id(b.r, b.c + 1)]
				+ at[plan.id(b.r + 1, b.c)]
				+ at[plan.id(b.r + 1, b.c + 1)]
			table.insert(points, { id = plan.BLOCK + plan.id(b.r, b.c), at = corners / 4, tag = b.use })
		end

		local data = buffer.create(#points * plan.POINT_BYTES)
		local tags = table.create(#points)
		for i, p in points do
			plan.write_point(data, i - 1, p.id, p.at)
			tags[i] = p.tag
		end
		return { count = #points, data = data, tags = tags }
	end,
})
