--!strict
-- Small helpers the game's scripts share.
local util = {}

local UP = vector.create(0, 1, 0)
local FORWARD = vector.create(0, 0, 1)
util.UP = UP

-- An entity's world position and rotation: its Transform composed with its
-- ancestors' (scripts read only the local Transform).
function util.world_pose(e: Entity): (vector, Quat)
	local position: vector = e:get("Transform", "translation")
	local rotation: Quat = e:get("Transform", "rotation")
	local parent = e:parent()
	while parent do
		if parent:has("Transform") then
			local pr: Quat = parent:get("Transform", "rotation")
			local ps: vector = parent:get("Transform", "scale")
			local pt: vector = parent:get("Transform", "translation")
			position = pt + pr * (position * ps)
			rotation = pr * rotation
		end
		parent = parent:parent()
	end
	return position, rotation
end

-- The way a rotation's +Z points, flattened onto the ground (unit length).
function util.heading(rotation: Quat): vector
	local f = rotation * FORWARD
	local flat = vector.create(f.x, 0, f.z)
	local len = vector.magnitude(flat)
	return if len > 1e-6 then flat / len else FORWARD
end

-- The yaw (radians about +Y) that turns +Z towards `dir`.
function util.yaw(dir: vector): number
	return math.atan2(dir.x, dir.z)
end

-- The signed angle from `a` to `b` about +Y, both on the ground, in
-- radians: positive turns left (towards +X from +Z).
function util.turn(a: vector, b: vector): number
	return math.atan2(a.z * b.x - a.x * b.z, a.x * b.x + a.z * b.z)
end

-- Distance between two points on the ground.
function util.flat_distance(a: vector, b: vector): number
	local dx, dz = a.x - b.x, a.z - b.z
	return math.sqrt(dx * dx + dz * dz)
end

-- `seconds` as m:ss.
function util.clock(seconds: number): string
	local s = math.max(0, math.ceil(seconds))
	return string.format("%d:%02d", s // 60, s % 60)
end

-- A whole number with thousands separated by commas.
function util.thousands(n: number): string
	local s = tostring(math.floor(math.abs(n)))
	local out = s:reverse():gsub("(%d%d%d)", "%1,"):reverse()
	if out:sub(1, 1) == "," then
		out = out:sub(2)
	end
	return if n < 0 then "-" .. out else out
end

-- Moves `current` towards `target` by the share of the way an exponential
-- approach at `rate` per second covers in `dt` seconds.
function util.approach(current: number, target: number, rate: number, dt: number): number
	return target + (current - target) * math.exp(-rate * dt)
end

function util.approach_vector(current: vector, target: vector, rate: number, dt: number): vector
	return target + (current - target) * math.exp(-rate * dt)
end

-- One of eight arrows for a turn (radians, positive left), for the HUD.
local ARROWS = { "↑", "↖", "←", "↙", "↓", "↘", "→", "↗" }
function util.arrow(turn: number): string
	local i = math.floor((turn / (math.pi / 4)) + 0.5) % 8
	return ARROWS[i + 1]
end

return util
