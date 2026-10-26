--!strict
-- The car: while the hero drives, the move stick's Y is the throttle (against
-- the way the car rolls it brakes first, then reverses or goes forward), its X
-- steers, and `brake` pulls the hand brake.
-- Parked, and while the hero gets out, the hand brake holds it.
--
-- The engine sound is three looping layers (children of the car on the
-- `vehicle` bus), made at 1020, 2820 and 4980 rpm (a four-cylinder firing 34,
-- 94 and 166 times a second): each plays fully at its own rpm and crossfades
-- with its neighbours' at equal power, louder with more throttle, and is
-- pitched to the engine's rpm. Parked, the engine is off. While the hero
-- tries to get out it runs on, with no throttle, so that it doesn't cut out
-- when there is no room and the hero drives on.
local rig = require("./lib/rig")

local car = script.entity :: Entity
local ENGINE = {
	{ part = world.get("engine_idle") :: Entity, rpm = 1020 },
	{ part = world.get("engine_mid") :: Entity, rpm = 2820 },
	{ part = world.get("engine_high") :: Entity, rpm = 4980 },
}
-- The Vehicle that player.luau adds when `rig.ready` turns true exists only
-- from the next step on, whichever script steps first.
local armed = false

-- Sets each layer's volume for the engine's `rpm` and `throttle` (0 to 1);
-- `level` 0 is off. The two layers around `rpm` share it linearly, and each
-- plays at the square root of its share, so the sum of their powers stays
-- the same through the crossfade. A layer that sounds plays `rpm / layer.rpm`
-- times as fast, so the two play the same firing tone; a silent one keeps its
-- last rate, so none plays more than 2.8 times as fast (a faster sound costs
-- more to mix).
local function engine_sound(rpm: number, throttle: number, level: number)
	local loud = level * (0.55 + 0.45 * throttle)
	for i = 1, #ENGINE do
		local layer = ENGINE[i]
		local below, above = ENGINE[i - 1], ENGINE[i + 1]
		local share = 1
		if rpm < layer.rpm and below then
			share = math.clamp((rpm - below.rpm) / (layer.rpm - below.rpm), 0, 1)
		elseif rpm > layer.rpm and above then
			share = math.clamp((above.rpm - rpm) / (above.rpm - layer.rpm), 0, 1)
		end
		layer.part:set("AudioSource", "volume", loud * math.sqrt(share))
		if loud > 0 and share > 0 then
			audio.set_rate(layer.part, rpm / layer.rpm)
		end
	end
end

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
		engine_sound(0, 0, 0)
		return
	end
	local state = physics.vehicle(car)
	if rig.leaving then
		physics.drive(car, { handbrake = 1 })
		engine_sound(state.rpm, 0, 1)
		return
	end
	local stick = input.axis("move")
	local speed = state.speed
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
	engine_sound(state.rpm, math.abs(throttle), 1)
end)
