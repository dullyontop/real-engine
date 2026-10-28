--!strict
-- Drives a vehicle along a route (lib/route, points about 5 m apart) by pure
-- pursuit: it steers towards a point a little way along the route ahead,
-- slows in time for the bends ahead and for the end, and backs out when it
-- is stuck. When the route starts behind it, it backs up to an end close by,
-- or turns round in the street: back round with the wheels the other way,
-- then forward. Each step `autopilot.step` turns the vehicle's state into
-- `physics.drive` input.
local util = require("./util")

local autopilot = {}

local CRUISE = 11.0 -- m/s, about 40 km/h
local CORNER = 5.5 -- m/s through a right-angle turn
local STOPPING = 2.6 -- m/s² of braking the speed plan allows for
local MAX_STEER = math.rad(34) -- the front wheels' max_steer_deg
local REACHED = 4.0 -- metres from a route point that count as past it
local PLAN_AHEAD = 50.0 -- metres of route the speed plan looks at
local TURN_ROUND = math.rad(115) -- a route this far round is behind
local TURNED = math.rad(70) -- backing round ends with the route this near ahead
local BACK_UP = 9.0 -- metres: an end this close behind is backed up to

-- What `physics.drive` takes.
export type Drive = { throttle: number?, steer: number?, brake: number?, handbrake: number? }

export type Pilot = {
	path: { vector },
	-- The next point of `path` to reach.
	index: number,
	-- Seconds spent stuck, and how long to keep reversing.
	stuck: number,
	reversing: number,
	-- Whether the reversing turns round (it ends once the route is ahead)
	-- rather than backs out of where the vehicle got stuck.
	turning: boolean,
	-- Whether the last point has been reached (within `arrive` metres).
	arrived: boolean,
	arrive: number,
}

function autopilot.new(path: { vector }, arrive: number?): Pilot
	return { path = path, index = 1, stuck = 0, reversing = 0, turning = false, arrived = false, arrive = arrive or 3.0 }
end

-- The point `ahead` metres along the path from `p`, starting at `index`.
local function lookahead(pilot: Pilot, p: vector, ahead: number): vector
	local from = p
	local left = ahead
	for i = pilot.index, #pilot.path do
		local to = pilot.path[i]
		local d = util.flat_distance(from, to)
		if d >= left then
			return from + (to - from) * (left / d)
		end
		left -= d
		from = to
	end
	return pilot.path[#pilot.path]
end

-- The drive input for this step: `at` and `rotation` are the vehicle's pose,
-- `speed` its forward speed (m/s).
function autopilot.step(pilot: Pilot, at: vector, rotation: Quat, speed: number, dt: number): Drive
	local path = pilot.path
	local heading = util.heading(rotation)
	-- Past a point once close to it, or once it is behind and the next one
	-- is nearer.
	while pilot.index < #path do
		local p = path[pilot.index]
		local to_p = vector.create(p.x - at.x, 0, p.z - at.z)
		local nearer = util.flat_distance(at, path[pilot.index + 1]) < util.flat_distance(p, path[pilot.index + 1])
		if vector.magnitude(to_p) < REACHED or (vector.dot(to_p, heading) < 0 and nearer) then
			pilot.index += 1
		else
			break
		end
	end
	local remaining = util.flat_distance(at, path[pilot.index])
	for i = pilot.index, #path - 1 do
		remaining += util.flat_distance(path[i], path[i + 1])
	end
	if remaining < pilot.arrive and math.abs(speed) < 1.0 then
		pilot.arrived = true
	end
	if pilot.arrived then
		return { brake = 1, handbrake = 1 }
	end

	local target = lookahead(pilot, at, 6 + 0.6 * math.max(speed, 0))
	local to_target = vector.create(target.x - at.x, 0, target.z - at.z)
	local turn = util.turn(heading, to_target)

	-- Reversing, to back out or to turn round: the wheels the other way
	-- swing the nose towards the route. It ends early when the vehicle cannot
	-- move back either.
	if pilot.reversing > 0 then
		pilot.reversing -= dt
		pilot.stuck = if math.abs(speed) < 0.3 then pilot.stuck + dt else 0
		if (pilot.turning and math.abs(turn) < TURNED) or pilot.stuck > 1.2 then
			pilot.reversing = 0
			pilot.stuck = 0
		else
			return { throttle = -0.6, steer = math.clamp(turn / MAX_STEER, -1, 1) }
		end
	end

	-- The route is behind: stop, then back up to its end when that is close
	-- (the tail steered towards it), or else turn round.
	if math.abs(turn) > TURN_ROUND then
		if speed > 0.8 then
			return { brake = 1 }
		end
		if remaining < BACK_UP then
			local back = util.turn(-heading, to_target)
			local throttle = if speed < -2 then 0 else -0.4
			return { throttle = throttle, steer = math.clamp(back / MAX_STEER * 1.3, -1, 1) }
		end
		pilot.reversing = 3.0
		pilot.turning = true
		pilot.stuck = 0
		return { brake = 1 }
	end

	-- physics.drive steers left with negative values.
	local steer = math.clamp(-turn / MAX_STEER * 1.3, -1, 1)

	-- The speed plan: cruise, but slow enough to take each bend within
	-- PLAN_AHEAD at its corner speed (braking at STOPPING), and stop at the end.
	local want = CRUISE
	local d = util.flat_distance(at, path[pilot.index])
	local prev = at
	for i = pilot.index, #path - 1 do
		local p, q = path[i], path[i + 1]
		local before = vector.create(p.x - prev.x, 0, p.z - prev.z)
		local after = vector.create(q.x - p.x, 0, q.z - p.z)
		if vector.magnitude(before) > 0.1 and vector.magnitude(after) > 0.1 then
			local bend = math.abs(util.turn(before, after))
			local corner = CRUISE - (CRUISE - CORNER) * math.clamp(bend / (math.pi / 2), 0, 1.4)
			want = math.min(want, math.sqrt(math.max(corner, 2) ^ 2 + 2 * STOPPING * math.max(d - 4, 0)))
		end
		d += vector.magnitude(after)
		prev = p
		if d > PLAN_AHEAD then
			break
		end
	end
	want = math.min(want, math.sqrt(2 * STOPPING * math.max(remaining - pilot.arrive * 0.5, 0)))
	-- Well off the route's way: crawl round.
	if math.abs(turn) > math.rad(60) then
		want = math.min(want, 3.5)
	end

	-- Stuck: pushing but not moving.
	if want > 1 and math.abs(speed) < 0.4 then
		pilot.stuck += dt
		if pilot.stuck > 1.5 then
			pilot.stuck = 0
			pilot.reversing = 1.6
			pilot.turning = false
		end
	else
		pilot.stuck = 0
	end

	local throttle, brake = 0, 0
	if speed < want - 0.3 then
		throttle = math.clamp((want - speed) * 0.35, 0.2, 1)
	elseif speed > want + 0.8 then
		brake = math.clamp((speed - want) * 0.25, 0.1, 1)
	end
	return { throttle = throttle, steer = steer, brake = brake }
end

return autopilot
