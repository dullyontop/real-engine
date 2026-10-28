--!strict
-- The camera, by `state.view`:
--   chase  behind and above the truck, looking a little ahead of it; it
--          swings round behind as the truck turns;
--   above  high over the truck, looking down at the streets around it;
--   orbit  circling the truck slowly (the title and results screens).
-- The chase and orbit cameras move in when a house would come between them
-- and the truck (a ray from the truck).
-- `camera` switches between chase and above during a shift.
local state = require("./lib/state")
local util = require("./lib/util")

local camera = script.entity :: Entity
local truck = world.get("truck") :: Entity

local eye = vector.create(0, 30, 40)
local look = vector.zero
local orbit = 0
local behind = vector.create(0, 0, -1)
-- The first frame jumps straight to the view.
local placed = false

input.on_pressed("camera", function()
	if state.mode == "shift" and not state.paused then
		state.view = if state.view == "chase" then "above" else "chase"
	end
end)

-- The rotation that looks from `from` towards `to` (+Y up).
local function looking(from: vector, to: vector): Quat
	return quat.look_at(to - from, util.UP)
end

-- `eye`, or nearer the truck at `at` when something is in between.
local function in_view(at: vector, eye: vector): vector
	local from = at + util.UP * 1.5
	local to = eye - from
	local hit = physics.raycast(from, vector.normalize(to), vector.magnitude(to))
	if hit and hit.entity ~= truck then
		return from + vector.normalize(to) * math.max(hit.distance - 0.6, 2.5)
	end
	return eye
end

time.on_frame(function(dt)
	dt = math.min(dt, 0.1)
	local at, rotation = util.world_pose(truck)
	local heading = util.heading(rotation)
	local want_eye, want_look
	if state.view == "chase" then
		-- Swing behind the way the truck moves (or faces, when slow).
		local back = -heading
		if state.speed < -1 then
			back = heading
		end
		behind = vector.normalize(util.approach_vector(behind, back, 3.0, dt))
		want_look = at + heading * 3 + util.UP * 1.2
		want_eye = in_view(at, at + behind * 10.5 + util.UP * 4.2)
	elseif state.view == "above" then
		want_look = at
		want_eye = at + vector.create(0, 70, 30)
	else
		orbit += dt * 0.12
		want_look = at + util.UP * 1.5
		want_eye = in_view(at, at + vector.create(math.sin(orbit) * 16, 7.5, math.cos(orbit) * 16))
	end
	if not placed then
		placed = true
		eye, look = want_eye, want_look
	else
		eye = util.approach_vector(eye, want_eye, 5.0, dt)
		look = util.approach_vector(look, want_look, 8.0, dt)
	end
	camera:set("Transform", "translation", eye)
	camera:set("Transform", "rotation", looking(eye, look))
end)
