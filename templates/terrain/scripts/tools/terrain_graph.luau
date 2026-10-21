--!strict
-- The landscape's ProcGraph (ADR-0015): a noise heightfield shaped into an
-- island, its terrain mesh and collider, and the sea. `graph.set_param`
-- changes a node's parameter (the `terrain-shape` recipe does), and
-- `real graph eval` shows what each node makes.
local g = proc.graph("terrain", { seed = 1 })

local height = g:node("terrain.heightfield", {
	size = 129, cell = 1.0, height = 16.0, scale = 0.018, octaves = 5,
}, nil, "height")
local mask = g:node("terrain.island_mask", { size = 129, cell = 1.0, falloff = 0.55 }, nil, "mask")
local land = g:node("field.combine", { op = "mul" }, { a = height, b = mask }, "land")

g:node("terrain.mesh", { collider = true, material = "materials/grass.material.json", name = "Terrain" }, { field = land }, "ground")
g:node("terrain.water_plane", { level = 1.2, size = 400.0, material = "materials/water.material.json", name = "Sea" }, nil, "water")

return g
