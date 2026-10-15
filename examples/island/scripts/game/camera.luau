--!strict
-- The camera rig: orbits the hero with the look stick or the mouse and follows
-- it, keeping a fixed distance.
local rig = require("./lib/rig")

local DISTANCE = 10.0
local camera = script.entity :: Entity

time.on_frame(function(_dt)
	-- Looked up each frame, so a restart's new hero is followed too.
	local hero = world.get("hero")
	if hero == nil then
		return
	end
	local look = input.axis("look")
	rig.yaw -= look.x
	rig.pitch = math.clamp(rig.pitch + look.y, 0.05, 1.2)
	local target: vector = hero:get("Transform", "translation") + vector.create(0, 1.2, 0)
	local back = vector.create(
		math.sin(rig.yaw) * math.cos(rig.pitch),
		math.sin(rig.pitch),
		math.cos(rig.yaw) * math.cos(rig.pitch)
	)
	local eye = target + back * DISTANCE
	camera:set("Transform", "translation", eye)
	camera:set("Transform", "rotation", quat.look_at(target - eye))
end)
