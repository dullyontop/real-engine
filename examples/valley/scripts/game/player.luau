--!strict
-- The hero: walks relative to the camera with the move stick, jumps, and
-- gets in or out of the car with `use`. The skinned body (the `hero_body`
-- child) faces the way it walks. Its Animator plays
-- `anims/walker.animgraph.json`, whose `speed` and `airborne` parameters are
-- set here every step, and a footstep sounds wherever a boot lands.
--
-- The valley streams in around the hero (its StreamFocus), so at the start
-- there is no ground yet. Once a ray finds ground under both the hero and the
-- car, both are set down on it: the hero gets its CharacterController, and
-- the car (kinematic in the scene) becomes a dynamic Vehicle.
local rig = require("./lib/rig")

local SPEED = 4.0
local REACH = 4.5 -- metres from the car's centre to get in
local CAR_RIDE = 1.0 -- metres from the ground to the car's centre on its wheels
local AIR_DELAY = 0.1 -- seconds in the air before the body shows it
local STEPS = { "assets/sfx/step_a.wav", "assets/sfx/step_b.wav" }
local LAND = "assets/sfx/land.wav"
local DOOR = "assets/sfx/door.wav"
-- The sole of a boot, from its lower-leg joint (the knee).
local SOLE = vector.create(0, -0.48, 0.04)
-- How far ahead of the hips a boot must swing for its landing to sound (a
-- full stride reaches 0.46 m).
local STEP_REACH = 0.2
local SIDES = { "L", "R" }
local UP = vector.create(0, 1, 0)
local DOWN = vector.create(0, -1, 0)
-- Where the hero gets out, from the car's centre in its own frame: by the
-- driver's door (the car's left, +X), else the other door, behind or in
-- front, but never where the car could reach the hero (`in_the_way`).
local EXITS = {
	vector.create(2.2, 0, 0),
	vector.create(-2.2, 0, 0),
	vector.create(0, 0, -3.1),
	vector.create(0, 0, 3.1),
}
-- 16 ways out from a spot to stand on.
local AROUND: { vector } = {}
for i = 1, 16 do
	local a = math.pi * i / 8
	AROUND[i] = vector.create(math.cos(a), 0, math.sin(a))
end
-- Rings round a spot, metres out, each with how far the underside of the
-- capsule's rounded foot is below the foot's middle there. The foot is a
-- sphere of the hero's radius, 0.35 m, and Jolt keeps 0.02 m of padding
-- round it, so 0.37 m in all.
local RINGS = {
	{ out = 0.175, depth = math.sqrt(0.37 * 0.37 - 0.175 * 0.175) },
	{ out = 0.35, depth = math.sqrt(0.37 * 0.37 - 0.35 * 0.35) },
}
-- How far up the ground's normal points on a 45° slope, the steepest the
-- hero walks (the CharacterController's max_slope_deg).
local STEEPEST = math.cos(math.rad(45))
-- How far up the ground's normal points on a 20° slope. On steeper ground the
-- parked car creeps downhill, even with the hand brake on, and from about
-- 30° it rolls or slides.
local SLIDES = math.cos(math.rad(20))

local hero = script.entity :: Entity
local body = world.get("hero_body") :: Entity
local car = world.get("car") :: Entity

local air = 0 -- seconds since the hero left the ground
local reach = { L = 0, R = 0 } -- each boot's reach ahead of the hips
local swinging = { L = false, R = false } -- whether it is moving forward
local fresh = true -- whether the boots' last reach is stale (after a fall or the car)
local steps = 0 -- footsteps so far, to alternate the two takes

body:add("Visible", { visible = true })

local function get_in()
	rig.driving = true
	hero:remove("CharacterController")
	body:set("Visible", "visible", false)
	anim.set(body, "speed", 0)
	anim.set(body, "airborne", false)
	audio.play(DOOR, car:get("Transform", "translation"))
	-- Inside, the wind is muffled.
	audio.snapshot("in_car", 0.4)
end

-- The height of the ground under `from`, or nil while none has loaded there.
local function ground_below(from: vector): number?
	local hit = physics.raycast(from, DOWN, 50)
	return if hit then hit.point.y else nil
end

-- The ground under the entity. The ray starts `below` metres under it, so
-- the car's own box is not the hit.
local function ground_under(e: Entity, below: number): number?
	return ground_below(e:get("Transform", "translation") - vector.create(0, below, 0))
end

-- Standing room `offset` from the car (in its frame, but level, so that a
-- tilted car still lets the hero out beside it), or nil. A ray from just
-- above the car's centre finds the ground at the spot: no higher than that
-- centre (not a bank, a tall rock, or a trunk the ray starts inside), no
-- more than 2 m below the wheels, and no steeper than 45°. The hero's
-- capsule would stand on it, its rounded foot just clear. Rays down on the
-- `RINGS` must find the ground (or a rock's top) below the foot's padded
-- underside there, and rays out from the foot's middle must meet nothing
-- within 0.5 m (ground up to 45° steep is farther out).
local function exit_at(pos: vector, rot: Quat, offset: vector): vector?
	local o = rot * offset
	local spot = pos + vector.normalize(vector.create(o.x, 0, o.z)) * vector.magnitude(offset)
	local from = vector.create(spot.x, pos.y + 0.5, spot.z)
	local hit = physics.raycast(from, DOWN, 50)
	if not hit or hit.point.y > pos.y or hit.point.y < pos.y - 3 or hit.normal.y < STEEPEST then
		return nil
	end
	-- The foot's middle, 0.4 m from the ground along its normal, so that the
	-- foot is 5 cm clear of it, slope or not.
	local foot = vector.create(spot.x, hit.point.y + 0.4 / hit.normal.y, spot.z)
	for i = 1, #AROUND do
		for _, ring in RINGS do
			local ground = ground_below(from + AROUND[i] * ring.out)
			if not ground or ground > foot.y - ring.depth then
				return nil
			end
		end
		if physics.raycast(foot, AROUND[i], 0.5) then
			return nil
		end
	end
	-- The capsule's middle (it is 1.8 m tall).
	return foot + vector.create(0, 0.55, 0)
end

-- Whether the car could reach a hero who got out `offset` from it (in its
-- frame). Where a wheel touches the ground says where it is. On wheels away
-- from the spot alone, with its side or end towards it raised, the car falls
-- back that way; with no wheel down, it comes down on a side or end it holds
-- more than about 15° up. Moving, it runs over a spot ahead of it (or behind
-- it, reversing). And on ground steeper than 20°, it creeps, rolls or slides
-- down the slope, so the spot must not lie below it.
local function in_the_way(pos: vector, rot: Quat, offset: vector): boolean
	local towards = rot * vector.normalize(offset)
	local state = physics.vehicle(car)
	local near, far = 0, 0
	for _, wheel in state.wheels do
		if wheel.on_ground then
			if vector.dot(wheel.contact_point - pos, towards) > 0 then
				near += 1
			else
				far += 1
			end
		end
	end
	if near == 0 and (far > 0 or towards.y > 0.25) then
		return true
	end
	if math.abs(state.speed) > 0.5 and offset.z * state.speed > 0 then
		return true
	end
	-- The ground under the car, from below its box (0.35 m from its centre to
	-- its floor), however it is tilted.
	local up = rot * UP
	local under = physics.raycast(pos - UP * (0.35 / math.max(up.y, 0.5) + 0.05), DOWN, 10)
	if not under or under.distance == 0 or under.normal.y > SLIDES then
		return false
	end
	-- Below the car is within 60° of straight down the slope.
	local down_slope = vector.normalize(vector.create(under.normal.x, 0, under.normal.z))
	return vector.dot(vector.normalize(vector.create(towards.x, 0, towards.z)), down_slope) > 0.5
end

local function get_out()
	local pos: vector = car:get("Transform", "translation")
	local rot: Quat = car:get("Transform", "rotation")
	local out: vector? = nil
	for i = 1, #EXITS do
		if not in_the_way(pos, rot, EXITS[i]) then
			out = exit_at(pos, rot, EXITS[i])
			if out then
				break
			end
		end
	end
	if not out then
		-- No room to stand anywhere by the car: the hero stays in, and the
		-- HUD says so.
		rig.no_room = time.now()
		return
	end
	hero:set("Transform", "translation", out)
	hero:add("CharacterController", { height = 1.8, radius = 0.35 })
	body:set("Visible", "visible", true)
	rig.driving = false
	rig.no_room = -math.huge
	air = 0
	fresh = true
	audio.play(DOOR, pos)
	audio.snapshot("default", 0.4)
end

local function start(hero_ground: number, car_ground: number)
	rig.ready = true
	local h: vector = hero:get("Transform", "translation")
	hero:set("Transform", "translation", vector.create(h.x, hero_ground + 0.95, h.z))
	hero:add("CharacterController", { height = 1.8, radius = 0.35 })
	local c: vector = car:get("Transform", "translation")
	car:set("Transform", "translation", vector.create(c.x, car_ground + CAR_RIDE, c.z))
	car:set("RigidBody", "kind", "Dynamic")
	-- A vehicle needs a dynamic chassis, so the car becomes one only now.
	car:add("Vehicle", { max_torque = 600, max_pitch_roll_deg = 60 })
	-- Face away from the camera.
	body:set("Transform", "rotation", quat.from_axis_angle(UP, rig.yaw + math.pi))
end

-- The sole of a boot in world space, from the last step's pose.
local function sole(side: string): vector
	local knee, rot = anim.joint(body, "LowerLeg." .. side)
	return knee + rot * SOLE
end

-- A boot lands as it stops swinging forward: when its reach ahead of the
-- hips, which grows through the swing, starts to shrink. After a fall or
-- the car, the boots are only measured, so that pose is no step.
local function footsteps()
	local hips, rot = anim.joint(body, "Hips")
	local forward = rot * vector.create(0, 0, 1)
	for i = 1, #SIDES do
		local side = SIDES[i]
		local foot = sole(side)
		local now = vector.dot(foot - hips, forward)
		local was = reach[side]
		if fresh then
			swinging[side] = false
		elseif swinging[side] and now < was - 0.0001 then
			swinging[side] = false
			if was > STEP_REACH then
				steps += 1
				audio.play(STEPS[steps % 2 + 1], foot, 0.8, "footsteps")
			end
		elseif not swinging[side] and now > was + 0.0001 then
			swinging[side] = true
		end
		reach[side] = now
	end
	fresh = false
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

time.on_step(function(dt)
	if not rig.ready then
		local h, c = ground_under(hero, 0), ground_under(car, 1)
		if h and c then
			start(h, c)
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
	if vector.magnitude(velocity) > 0.1 then
		-- The model faces +Z; turn it towards the walking direction.
		local facing = math.atan2(velocity.x, velocity.z)
		body:set("Transform", "rotation", quat.from_axis_angle(UP, facing))
	end
	local state = physics.character(hero)
	if state.ground == "in_air" then
		air += dt
	else
		if air > 0.3 then
			audio.play(LAND, state.position - vector.create(0, 0.9, 0), 1, "footsteps")
		end
		-- A fall (the body showed it) leaves the boots' reach stale; after a
		-- shorter hop, a boot that landed meanwhile still sounds, late.
		if air > AIR_DELAY then
			fresh = true
		end
		air = 0
	end
	-- The legs follow how fast the hero really goes, so walking into a
	-- slope too steep to climb does not run on the spot.
	local v = state.velocity
	anim.set(body, "speed", math.sqrt(v.x * v.x + v.z * v.z))
	anim.set(body, "airborne", air > AIR_DELAY)
	if air == 0 then
		footsteps()
	end
end)
