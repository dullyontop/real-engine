--!strict
-- The car: while the hero drives, the move stick's Y is the throttle (back
-- brakes, then reverses), its X steers, and `brake` pulls the hand brake.
-- Parked, the hand brake holds it.
local rig = require("./lib/rig")

local car = script.entity :: Entity

time.on_step(function(_dt)
	if not rig.driving then
		physics.drive(car, { handbrake = 1 })
		return
	end
	local stick = input.axis("move")
	local speed = physics.vehicle(car).speed
	local throttle = stick.y
	local brake = 0
	-- Pulling back while still rolling forward brakes before it reverses.
	if throttle < 0 and speed > 1 then
		brake = -throttle
		throttle = 0
	end
	physics.drive(car, {
		throttle = throttle,
		steer = -stick.x,
		brake = brake,
		handbrake = if input.down("brake") then 1 else 0,
	})
end)
