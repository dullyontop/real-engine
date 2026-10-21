--!strict
-- The car: while the hero drives, the move stick's Y is the throttle (against
-- the way the car rolls it brakes first, then reverses or goes forward), its X
-- steers, and `brake` pulls the hand brake.
-- Parked, the hand brake holds it.
local rig = require("./lib/rig")

local car = script.entity :: Entity
-- The Vehicle that player.luau adds when `rig.ready` turns true exists only
-- from the next step on, whichever script steps first.
local armed = false

time.on_step(function(_dt)
	if not rig.ready then
		return
	end
	if not armed then
		armed = true
		return
	end
	if not rig.driving then
		physics.drive(car, { handbrake = 1 })
		return
	end
	local stick = input.axis("move")
	local speed = physics.vehicle(car).speed
	local throttle = stick.y
	local brake = 0
	-- Against the way it rolls, the stick brakes first; it reverses (or
	-- goes forward again) only once the car has nearly stopped.
	if throttle < 0 and speed > 1 then
		brake = -throttle
		throttle = 0
	elseif throttle > 0 and speed < -1 then
		brake = throttle
		throttle = 0
	end
	physics.drive(car, {
		throttle = throttle,
		steer = stick.x,
		brake = brake,
		handbrake = if input.down("brake") then 1 else 0,
	})
end)
