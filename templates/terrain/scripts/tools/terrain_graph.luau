--!strict
-- The landscape's ProcGraph (ADR-0015): a noise heightfield shaped into an
-- island, its terrain mesh and collider, and the sea. Its params, `height`
-- and `roughness`, are what the inspector shows and what
-- `graph.set_param { source, param, value }` sets for a source (the
-- `terrain-shape` recipe does); `real graph eval` shows what each node makes.
local g = proc.graph("terrain", { seed = 1 })

local hills = g:param("height", {
	type = "number", default = 16.0, min = 1.0, max = 64.0,
	doc = "The height of the highest hills, in metres.",
})
local roughness = g:param("roughness", {
	type = "number", default = 0.018, min = 0.002, max = 0.1,
	doc = "The noise scale: small values give broad hills, large ones rough ground.",
})

local height = g:node("terrain.heightfield", {
	size = 129, cell = 1.0, height = hills, scale = roughness, octaves = 5,
}, nil, "height")
local mask = g:node("terrain.island_mask", { size = 129, cell = 1.0, falloff = 0.55 }, nil, "mask")
local land = g:node("field.combine", { op = "mul" }, { a = height, b = mask }, "land")

g:node("terrain.mesh@3", { collider = true, material = "materials/grass.material.json", name = "Terrain" }, { field = land }, "ground")
g:node("terrain.water_plane", { level = 1.2, size = 400.0, material = "materials/water.material.json", name = "Sea" }, nil, "water")

return g
