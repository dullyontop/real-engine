--!strict
-- The courier's truck. Whoever `state.driver` names drives it each step:
-- the player with the `drive` stick (its Y is the throttle: against the way
-- the truck rolls it brakes first, then reverses or goes forward; its X
-- steers) and `brake` for the hand brake, the autopilot along
-- `state.pilot`'s route, or nobody (parked, hand brake on).
--
-- It also measures knocks for the cargo (`state.jolt`: how much the truck's
-- velocity changed in one step beyond what driving does), sounds the horn
-- and the crashes, and puts the truck back on its wheels on `reset`. A
-- teleport (`state.teleport`) stops the truck where it is put, and the drop
-- onto its wheels after one is not a knock.
--
-- The engine sound is three looping layers (children of the truck on the
-- `engine` bus) made at 1020, 2580 and 4380 rpm (a four-cylinder diesel
-- firing 34, 86 and 146 times a second, tools/make_sounds.py): each plays
-- fully at its own rpm and crossfades with its neighbours' at equal power,
-- louder with more throttle, and is pitched to the engine's rpm.
local state = require("./lib/state")
local autopilot = require("./lib/autopilot")
local util = require("./lib/util")

local truck = script.entity :: Entity
local ENGINE = {
	{ part = world.get("engine_idle") :: Entity, rpm = 1020 },
	{ part = world.get("engine_mid") :: Entity, rpm = 2580 },
	{ part = world.get("engine_high") :: Entity, rpm = 4380 },
}
-- A change of velocity in one step (m/s) that driving alone does not give:
-- hard braking is about 0.15, a kerb at speed about 0.5.
local KNOCK = 0.6
local CRASH_SOUND = 1.2
-- Seconds after a teleport in which nothing counts as a knock.
local SETTLE = 0.75

local last_velocity = vector.zero
local settling = 0
local horn = false
local reset = false

input.on_pressed("horn", function()
	horn = true
end)
input.on_pressed("reset", function()
	reset = true
end)

-- Each layer's volume for the engine's `rpm` and `throttle` (0 to 1); `level`
-- 0 is off. The two layers around `rpm` share it, each at the square root of
-- its share, so the sum of their powers stays the same through the
-- crossfade; a sounding layer plays `rpm / layer.rpm` times as fast.
local function engine_sound(rpm: number, throttle: number, level: number)
	local loud = level * (0.5 + 0.5 * throttle)
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
			audio.set_rate(layer.part, math.clamp(rpm / layer.rpm, 0.5, 2.5))
		end
	end
end

-- The player's input as drive input: the stick brakes against the way the
-- truck rolls before it reverses or goes forward again.
local function player_input(speed: number): autopilot.Drive
	local stick = input.axis("drive")
	local throttle = stick.y
	local brake = 0
	if throttle < 0 and speed > 1 then
		brake = -throttle
		throttle = 0
	elseif throttle > 0 and speed < -1 then
		brake = throttle
		throttle = 0
	end
	return {
		throttle = throttle,
		steer = stick.x,
		brake = brake,
		handbrake = if input.down("brake") then 1 else 0,
	}
end

time.on_step(function(dt)
	local at, rotation = util.world_pose(truck)
	if state.teleport then
		local to = state.teleport
		state.teleport = nil
		truck:set("Transform", "translation", to.at)
		truck:set("Transform", "rotation", to.rotation)
		physics.set_velocity(truck, vector.zero, vector.zero)
		physics.drive(truck, { brake = 1, handbrake = 1 })
		last_velocity = vector.zero
		state.jolt = 0
		settling = SETTLE
		return
	end

	local v = physics.vehicle(truck)
	state.speed, state.rpm, state.gear = v.speed, v.rpm, v.gear
	local linear = physics.velocity(truck)
	local change = vector.magnitude(linear - last_velocity)
	last_velocity = linear
	if settling > 0 then
		settling -= dt
		change = 0
	end
	state.jolt = math.max(0, change - KNOCK)
	if change > CRASH_SOUND then
		audio.play("assets/sfx/crash.wav", at, math.clamp(change / 5, 0.3, 1), "sfx")
	end

	if reset then
		reset = false
		if state.driver == "player" then
			-- Upright, facing the way it faced, a little above where it is.
			local yaw = util.yaw(util.heading(rotation))
			state.teleport = { at = at + util.UP * 1.5, rotation = quat.from_axis_angle(util.UP, yaw) }
		end
	end
	if horn then
		horn = false
		if state.driver == "player" then
			audio.play("assets/sfx/horn.wav", at, 0.9, "sfx")
		end
	end

	local drive: autopilot.Drive
	local level = 1
	if state.driver == "player" then
		drive = player_input(v.speed)
	elseif state.driver == "autopilot" and state.pilot then
		drive = autopilot.step(state.pilot, at, rotation, v.speed, dt)
	else
		drive = { brake = 1, handbrake = 1 } :: autopilot.Drive
		level = 0.6
	end
	physics.drive(truck, drive)
	engine_sound(v.rpm, math.abs(drive.throttle or 0), level)
end)
