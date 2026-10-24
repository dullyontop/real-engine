--!strict
-- The island's ProcGraph (ADR-0015): a noise heightfield shaped into an island,
-- its terrain mesh and collider, the sea, gems to collect and crates to push.
-- `real graph eval` and the editor's graph panel show what each node makes.
local g = proc.graph("island", { seed = 1 })

local height = g:node("terrain.heightfield", {
	size = 129, cell = 1.0, height = 16.0, scale = 0.018, octaves = 5,
}, nil, "height")
local mask = g:node("terrain.island_mask", { size = 129, cell = 1.0, falloff = 0.55 }, nil, "mask")
local land = g:node("field.combine", { op = "mul" }, { a = height, b = mask }, "land")

g:node("terrain.mesh@2", { collider = true, material = "materials/grass.material.json", name = "Terrain" }, { field = land }, "ground")
g:node("terrain.water_plane", { level = 1.2, size = 400.0, material = "materials/water.material.json", name = "Sea" }, nil, "water")

-- Gems on dry, gentle ground, far enough apart to make the player walk.
local gem_spots = g:node("scatter.poisson", {
	radius = 18.0, height_min = 3.0, slope_max = 0.6,
}, { field = land }, "gem_spots")
g:node("points.instance", { prefab = "prefabs/collectible.prefab.json", offset_y = 0.8 }, { points = gem_spots }, "gems")

-- Crates to push around.
local crate_spots = g:node("scatter.poisson", {
	radius = 24.0, height_min = 3.0, slope_max = 0.4,
}, { field = land }, "crate_spots")
g:node("points.instance", { prefab = "prefabs/crate.prefab.json", random_yaw = true, offset_y = 0.1 }, { points = crate_spots }, "crates")

return g
