--!strict
-- The hero: walks relative to the camera with the move stick, jumps, and
-- gets in or out of the car with `use`. The skinned body (the `hero_body`
-- child) faces the way it walks and plays `walk` or `idle`.
--
-- The valley streams in around the hero (its StreamFocus), so at the start
-- there is no ground yet. The hero gets its CharacterController, and the car
-- (kinematic in the scene) becomes a dynamic Vehicle, once a ray finds ground
-- under both.
local rig = require("./lib/rig")

local SPEED = 4.0
local REACH = 4.5 -- metres from the car's centre to get in
local WALK = "models/walker.glb#walk"
local IDLE = "models/walker.glb#idle"
local STRIDE = 1.6 -- metres per second the walk clip covers at speed 1

local hero = script.entity :: Entity
local body = world.get("hero_body") :: Entity
local car = world.get("car") :: Entity

local function play(clip: string, speed: number)
	if body:get("AnimPlayer", "clip") ~= clip then
		body:set("AnimPlayer", "clip", clip)
	end
	body:set("AnimPlayer", "speed", speed)
end

local function get_in()
	rig.driving = true
	hero:remove("CharacterController")
	body:set("Visible", "visible", false)
	play(IDLE, 1)
end

local function get_out()
	-- Out on the driver's side (the car's left, +X in its own frame), a
	-- little above the ground so the capsule drops onto it.
	local pos: vector = car:get("Transform", "translation")
	local rot: Quat = car:get("Transform", "rotation")
	hero:set("Transform", "translation", pos + rot * vector.create(2.2, 0.8, 0))
	hero:add("CharacterController", { height = 1.8, radius = 0.35 })
	body:set("Visible", "visible", true)
	rig.driving = false
end

body:add("Visible", { visible = true })

local DOWN = vector.create(0, -1, 0)

-- From `below` metres under the entity, so the car's own box is not the hit.
local function ground_under(e: Entity, below: number): boolean
	local from: vector = e:get("Transform", "translation") - vector.create(0, below, 0)
	return physics.raycast(from, DOWN, 50) ~= nil
end

local function start()
	rig.ready = true
	hero:add("CharacterController", { height = 1.8, radius = 0.35 })
	car:set("RigidBody", "kind", "Dynamic")
	-- A vehicle needs a dynamic chassis, so the car becomes one only now.
	car:add("Vehicle", { max_torque = 600, max_pitch_roll_deg = 60 })
	-- Face away from the camera.
	body:set("Transform", "rotation", quat.from_axis_angle(vector.create(0, 1, 0), rig.yaw + math.pi))
end

input.on_pressed("use", function()
	if not rig.ready then
		return
	end
	if rig.driving then
		get_out()
		return
	end
	local d = vector.magnitude(car:get("Transform", "translation") - hero:get("Transform", "translation"))
	if d < REACH then
		get_in()
	end
end)

time.on_step(function(_dt)
	if not rig.ready then
		if ground_under(hero, 0) and ground_under(car, 1) then
			start()
		end
		return
	end
	if rig.driving then
		-- The hero rides along, so the streaming focus follows the car.
		hero:set("Transform", "translation", car:get("Transform", "translation") + vector.create(0, 1, 0))
		return
	end
	local stick = input.axis("move")
	local forward = vector.create(-math.sin(rig.yaw), 0, -math.cos(rig.yaw))
	local right = vector.create(math.cos(rig.yaw), 0, -math.sin(rig.yaw))
	local velocity = (forward * stick.y + right * stick.x) * SPEED
	physics.move_character(hero, velocity, input.down("jump"))
	local speed = vector.magnitude(velocity)
	if speed > 0.1 then
		-- The model faces +Z; turn it towards the walking direction.
		local facing = math.atan2(velocity.x, velocity.z)
		body:set("Transform", "rotation", quat.from_axis_angle(vector.create(0, 1, 0), facing))
		play(WALK, speed / STRIDE)
	else
		play(IDLE, 1)
	end
end)
