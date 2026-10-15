local g = proc.graph("island", { seed = 1234 })
local h = g:node("terrain.heightfield", { size = 64, octaves = 5 })
local m = g:node("terrain.island_mask", { falloff = 0.6 })
local t = g:node("field.combine", { op = "mul" }, { a = h, b = m })
g:node("terrain.mesh", { cell = 1.0 }, { field = t:out("out") })
return g
