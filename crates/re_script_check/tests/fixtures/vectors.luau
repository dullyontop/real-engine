--!strict
-- Native vector values and the vector library.
local up = vector.create(0, 1, 0)
local v = vector.create(3, 4, 0)
local len: number = vector.magnitude(v)
local n = vector.normalize(v)
local d = vector.dot(n, up) + len
local spawned = world.spawn({ components = { Transform = { translation = v * 2 + up } } })
print(d, spawned.id)
