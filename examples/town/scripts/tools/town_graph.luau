--!strict
-- The town's ProcGraph (ADR-0015). The project's own node kinds
-- (scripts/generators/) lay out the streets and build the houses; the
-- engine's nodes make the roads, pavements, ground, trees and lamps:
--
--   project.town_plan  the junctions and what each block is for
--   project.street     each row and column of junctions as a road path
--   road.mesh          the asphalt, with a surface for every junction
--   road.footprint     the roads' area, and the area with the pavements
--   shape.boolean      the pavements (that area minus the roads) and the
--                      blocks (the town minus both)
--   terrain.conform    the hills, flattened under the town and its roads
--   project.town       houses with addresses, the depot, junction markers
--   project.greenery   trees in the parks and back gardens, lamps by the
--                      kerbs, placed by points.instance
--
-- `real graph eval` and the editor's graph panel show what each node makes.
local g = proc.graph("town", { seed = 1 })

local ROWS, COLS, SPACING = 5, 5, 64
local GROUND = 6.0 -- the town's ground; the roads are 5 cm above it
local half = (COLS - 1) / 2 * SPACING -- from the middle to the edge streets

local plan = g:node("project.town_plan", { rows = ROWS, cols = COLS, spacing = SPACING, level = GROUND + 0.05 }, nil, "plan")

-- One path per street, and a table of them all for the nodes that take
-- every road on any port.
local streets: { [string]: GraphNode } = {}
for r = 0, ROWS - 1 do
	streets[`row{r}`] = g:node("project.street", { axis = "row", index = r }, { plan = plan }, `street_row{r}`)
end
for c = 0, COLS - 1 do
	streets[`col{c}`] = g:node("project.street", { axis = "col", index = c }, { plan = plan }, `street_col{c}`)
end
local function with_streets(inputs: { [string]: GraphNode | GraphPort }): { [string]: GraphNode | GraphPort }
	for port, s in streets do
		inputs[port] = s
	end
	return inputs
end

-- Roads and pavements.
g:node("road.mesh", { junction_radius = 9.0, material = "materials/asphalt.material.json" }, with_streets({}), "roads")
local kerbs = g:node("road.footprint", { margin = 0.0 }, with_streets({}), "kerbs")
local walks = g:node("road.footprint", { margin = 2.5 }, with_streets({}), "walks")
local pavement = g:node("shape.boolean", { op = "difference" }, { a = walks, b = kerbs }, "pavement")
g:node("shape.mesh", { height = GROUND + 0.03, material = "materials/pavement.material.json" }, { shapes = pavement }, "pavements")

-- The town's square, and the blocks inside it.
local edge = half + 2
local town = g:node("shape.polygon", {
	points = { { -edge, -edge }, { edge, -edge }, { edge, edge }, { -edge, edge } },
}, nil, "town")
local blocks = g:node("shape.boolean", { op = "difference" }, { a = town, b = walks }, "blocks")

-- Houses, shops, the depot, fountains and the junction markers.
g:node("project.town", { ground = GROUND }, { plan = plan, blocks = blocks }, "buildings")

-- Trees and bushes (random turns and sizes), and street lamps (upright).
local green = g:node("project.greenery", { ground = GROUND }, { plan = plan, blocks = blocks }, "greenery")
g:node("points.instance", {
	prefabs = { tree = "prefabs/tree.prefab.json", bush = "prefabs/bush.prefab.json" },
	random_yaw = true, scale_min = 0.8, scale_max = 1.25,
}, { points = green }, "trees")
g:node("points.instance", {
	prefabs = { lamp = "prefabs/lamp.prefab.json" }, random_yaw = false,
}, { points = green }, "lamps")

-- The ground: hills all round, flat where the town is. `outskirts` is 0 in
-- the town and rises to 1 over 70 m beyond it.
local hills = g:node("terrain.heightfield", {
	size = 257, cell = 2.0, scale = 0.0065, height = 34.0, octaves = 5,
}, nil, "hills")
local around = g:node("shape.offset", { distance = 18.0, join = "round" }, { shapes = town }, "around")
local outskirts = g:node("field.from_shapes", { falloff = 70.0, invert = true }, { shapes = around, field = hills }, "outskirts")
local rise = g:node("field.combine", { op = "mul" }, { a = hills, b = outskirts }, "rise")
local level = g:node("field.remap", {
	mode = "linear", in_min = 0.0, in_max = 1.0, out_min = GROUND, out_max = GROUND + 1.0,
}, { field = rise }, "level")
local graded = g:node("terrain.conform", { shoulder = 1.0, blend = 6.0 }, with_streets({ field = level }), "graded")
g:node("terrain.mesh@3", { collider = true, material = "materials/grass.material.json", name = "Ground" }, { field = graded }, "ground")

return g
