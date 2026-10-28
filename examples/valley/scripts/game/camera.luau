--!strict
-- The camera rig: orbits the hero (or the car while driving) with the look
-- stick or the mouse, and follows it at a fixed distance. It turns with the
-- input each frame, and moves in the late frame, after the fixed steps, to
-- where the hero or the car is drawn, so neither shakes against it.
local rig = require("./lib/rig")

local camera = script.entity :: Entity
local hero = world.get("hero") :: Entity
local car = world.get("car") :: Entity

time.on_frame(function(_dt)
	local look = input.axis("look")
	rig.yaw -= look.x
	rig.pitch = math.clamp(rig.pitch - look.y, 0.05, 1.2)
end)

time.on_late_frame(function(_dt)
	local target: vector
	local distance: number
	if rig.driving then
		target = car:drawn_transform() + vector.create(0, 1.5, 0)
		distance = 9.0
	else
		target = hero:drawn_transform() + vector.create(0, 0.9, 0)
		distance = 6.0
	end
	local back = vector.create(
		math.sin(rig.yaw) * math.cos(rig.pitch),
		math.sin(rig.pitch),
		math.cos(rig.yaw) * math.cos(rig.pitch)
	)
	local eye = target + back * distance
	camera:set("Transform", "translation", eye)
	camera:set("Transform", "rotation", quat.look_at(target - eye))
end)
