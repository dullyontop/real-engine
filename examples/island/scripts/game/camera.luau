--!strict
-- The camera rig: orbits the hero with the look stick or the mouse and follows
-- it, keeping a fixed distance. It turns with the input each frame, and moves
-- in the late frame, after the fixed steps, to where the hero is drawn, so it
-- never trails the hero by a step.
local rig = require("./lib/rig")

local DISTANCE = 10.0
local camera = script.entity :: Entity

time.on_frame(function(_dt)
	local look = input.axis("look")
	rig.yaw -= look.x
	rig.pitch = math.clamp(rig.pitch + look.y, 0.05, 1.2)
end)

time.on_late_frame(function(_dt)
	-- Looked up each frame, so a restart's new hero is followed too.
	local hero = world.get("hero")
	if hero == nil then
		return
	end
	local at = hero:drawn_transform()
	local target: vector = at + vector.create(0, 1.2, 0)
	local back = vector.create(
		math.sin(rig.yaw) * math.cos(rig.pitch),
		math.sin(rig.pitch),
		math.cos(rig.yaw) * math.cos(rig.pitch)
	)
	local eye = target + back * DISTANCE
	camera:set("Transform", "translation", eye)
	camera:set("Transform", "rotation", quat.look_at(target - eye))
end)
