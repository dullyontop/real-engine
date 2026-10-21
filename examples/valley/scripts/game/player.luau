--!strict
-- The hero: walks relative to the camera with the move stick, jumps, and
-- gets in or out of the car with `use`. The skinned body (the `hero_body`
-- child) faces the way it walks and plays `walk` or `idle`.
local rig = require("./lib/rig")

local SPEED = 4.0
local REACH = 4.5 -- metres from the car's centre to get in
local WALK = "models/walker.glb#walk"
local IDLE = "models/walker.glb#idle"
local STRIDE = 1.6 -- metres per second the walk clip covers at speed 1

local hero = script.entity :: Entity
local body = world.get("hero_body") :: Entity
local car = world.get("car") :: Entity
local facing = rig.yaw

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

input.on_pressed("use", function()
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
	if rig.driving then
		-- The hero rides along, so whatever follows it (the camera, later
		-- the streaming focus) follows the car.
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
		facing = math.atan2(velocity.x, velocity.z)
		body:set("Transform", "rotation", quat.from_axis_angle(vector.create(0, 1, 0), facing))
		play(WALK, speed / STRIDE)
	else
		play(IDLE, 1)
	end
end)
