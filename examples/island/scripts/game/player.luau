--!strict
-- The third-person character: the move stick walks relative to the camera,
-- jump jumps when on the ground.
local rig = require("./lib/rig")

local SPEED = 6.0
local hero = script.entity :: Entity

time.on_step(function(_dt)
	local stick = input.axis("move")
	local forward = vector.create(-math.sin(rig.yaw), 0, -math.cos(rig.yaw))
	local right = vector.create(math.cos(rig.yaw), 0, -math.sin(rig.yaw))
	local velocity = (forward * stick.y + right * stick.x) * SPEED
	physics.move_character(hero, velocity, input.down("jump"))
end)
