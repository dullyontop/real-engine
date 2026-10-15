--!strict
-- A small module: helpers over native vectors.
local M = {}

function M.lerp(a: vector, b: vector, t: number): vector
	return a + (b - a) * t
end

function M.flat(v: vector): vector
	return vector.create(v.x, 0, v.z)
end

return M
