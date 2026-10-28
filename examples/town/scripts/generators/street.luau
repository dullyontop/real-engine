--!strict
-- One street of the town plan as a road path: the spline through its row or
-- column of junctions, carried on to the outer edge of the streets it ends
-- on. Feed every street to `road.mesh`, `road.footprint` and
-- `terrain.conform`, which find the junctions where they cross.
local plan = require("lib/plan")

return proc.node({
	kind = "project.street",
	version = 1,
	doc = "One street of a project.town_plan: its row or column of junctions as a road path.",
	inputs = { { name = "plan", type = "points", required = true, doc = "The junctions (project.town_plan)." } },
	output = "path",
	params = {
		axis = proc.param.enum({ values = { "row", "col" }, default = "row", doc = "A row (runs east-west) or a column (runs north-south)." }),
		index = proc.param.integer({ default = 0, min = 0, max = 8, doc = "Which row or column." }),
		step = proc.param.number({ default = 2, min = 0.5, max = 10, doc = "Metres between samples." }),
	},
	eval = function(_ctx, inputs, params)
		local p = plan.read(inputs.plan)
		for _, s in plan.streets(p) do
			if s.axis == params.axis and s.index == params.index then
				local extend = 9 / 2 -- half an edge street's width
				local width = plan.width(s)
				local samples = {}
				for i, at in plan.centreline(s, params.step, extend) do
					samples[i] = { x = at.x, y = at.y, z = at.z, width = width, bank = 0 }
				end
				return { closed = false, samples = samples }
			end
		end
		error(`the plan has no {params.axis} {params.index}`)
	end,
})
