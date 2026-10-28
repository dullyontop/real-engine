--!strict
-- Town Courier: deliver parcels around a procedurally generated town before
-- the shift ends.
--
-- The title screen shows the town while the autopilot drives the truck
-- around it. `start` begins a shift (`Rules.shift_seconds` long) at the
-- depot: stop in the loading bay and `Rules.parcels` parcels are loaded,
-- each for an address in town, marked by a yellow beacon. Stop at an
-- address's kerb to deliver: points for each parcel, more for a quick
-- delivery and for one that never fell off. Knocks throw parcels off the
-- bed (lib/cargo); stop next to one to pick it up, or lose it after a
-- while. With the bed empty, the depot (a green beacon) has more. When the
-- time is up the results show, and `start` builds a new town (a new seed)
-- for the next shift. `pause` pauses a shift: the simulation stands still
-- (time.set_scale) and the music is muffled (the `paused` snapshot).
--
-- With `Rules.autopilot` set, the autopilot works the shift too (the
-- replays in tests/replays/ use it to play whole shifts the same way every
-- run). All the game's choices use math.random, which is seeded, so a run
-- repeats exactly.
local state = require("./lib/state")
local town = require("./lib/town")
local route = require("./lib/route")
local autopilot = require("./lib/autopilot")
local cargo = require("./lib/cargo")
local util = require("./lib/util")

local game = script.entity :: Entity
local truck = world.get("truck") :: Entity
local sun = world.get("sun") :: Entity
local hud = {
	status = world.get("hud_status") :: Entity,
	jobs = world.get("hud_jobs") :: Entity,
	street = world.get("hud_street") :: Entity,
	message = world.get("hud_message") :: Entity,
	help = world.get("hud_help") :: Entity,
	speed = world.get("hud_speed") :: Entity,
}

local DELIVER_RADIUS = 7.0 -- metres from the kerbside spot
local DELIVER_SPEED = 2.5 -- m/s, slow enough to hand a parcel over
local BAY_RADIUS = 6.0
local FAR_FROM_DEPOT = 60 -- metres: no jobs next door to the depot
local START_HOUR, END_HOUR = 9, 17

type Job = {
	id: number,
	address: town.Address,
	loaded: number, -- time.now() when loaded
	fell: boolean, -- whether its parcel fell off on the way
	done: boolean,
	beacon: Entity?,
}

local jobs: { Job } = {}
local next_job = 1
local score, delivered, lost_count = 0, 0, 0
local left = 0 -- seconds of the shift left
local best = 0
local message, message_until = "", 0
local depot_beacon: Entity? = nil
local beacons = 0
local hurry = false
-- Seconds left when the hurry starts: the last quarter of a shift, at most
-- HURRY seconds.
local HURRY = 30
local hurry_at = HURRY
-- The town has been read (lib/town's refresh takes a few steps).
local ready = false
-- A new town is being built and read.
local building = false

-- The message line's colours.
local WHITE = color.new(1, 1, 1)
local TONES = {
	info = WHITE,
	good = color.new(0.55, 1, 0.5),
	bad = color.new(1, 0.45, 0.4),
	warn = color.new(1, 0.85, 0.3),
}
local message_tone = WHITE

local function rules(field: string): any
	return game:get("Rules", field)
end

local function say(text: string, seconds: number?, tone: ("info" | "good" | "bad" | "warn")?)
	message = text
	message_until = time.now() + (seconds or 3)
	message_tone = TONES[tone or "info"]
end

-- Waits until the next frame has begun (a regeneration is applied at the end
-- of the frame that asked for it).
local function next_frame()
	local f = time.frame()
	repeat
		task.wait()
	until time.frame() > f
end

local function beacon(at: vector, material: string): Entity
	beacons += 1
	return world.spawn({
		id = `beacon_{beacons}`,
		name = "Beacon",
		components = {
			Transform = { translation = at + vector.create(0, 5, 0), scale = vector.create(1.6, 5, 1.6) },
			MeshRef = { asset = "builtin:capsule" },
			MaterialRef = { asset = material },
		},
	})
end

local function show_depot(show: boolean)
	if show and not depot_beacon then
		depot_beacon = beacon(town.bay, "materials/depot_beacon.material.json")
	elseif not show and depot_beacon then
		(depot_beacon :: Entity):destroy()
		depot_beacon = nil
	end
end

local function drop_job_beacon(job: Job)
	if job.beacon then
		job.beacon:destroy()
		job.beacon = nil
	end
end

-- The sun for an hour of the day: up in the east-south-east in the morning,
-- high at noon, low and orange by five.
local function set_daylight(hour: number)
	local t = math.clamp((hour - 6) / 13, 0, 1) -- 06:00 to 19:00
	local azimuth = math.rad(100 + 160 * t)
	local elevation = math.rad(8 + 52 * math.sin(math.pi * t))
	local to_sun = vector.create(
		math.cos(elevation) * math.sin(azimuth),
		math.sin(elevation),
		math.cos(elevation) * math.cos(azimuth)
	)
	sun:set("Transform", "rotation", quat.look_at(-to_sun, util.UP))
	local low = math.clamp(1 - math.deg(elevation) / 30, 0, 1)
	sun:set("Light", "color", color.new(1, 1 - 0.25 * low, 1 - 0.45 * low))
	sun:set("Light", "intensity", 0.75 + 0.45 * (1 - low))
end

local function hour_now(): number
	if state.mode == "shift" then
		local length: number = rules("shift_seconds")
		return START_HOUR + (END_HOUR - START_HOUR) * (1 - left / length)
	elseif state.mode == "results" then
		return END_HOUR
	end
	return 10.5
end

-- Puts the truck where the town says it starts, facing the street.
local function to_spawn()
	state.teleport = {
		at = town.spawn,
		rotation = town.spawn_rotation or quat.identity(),
	}
end

-- Where the truck is, or is about to be put.
local function truck_pose(): (vector, Quat)
	local to = state.teleport
	if to then
		return to.at, to.rotation
	end
	return util.world_pose(truck)
end

-- The autopilot heads for `to`.
local function pilot_to(to: vector, arrive: number?)
	local at, rotation = truck_pose()
	state.pilot = autopilot.new(route.plan(at, util.heading(rotation), to), arrive)
end

local function wander()
	pilot_to(route.random_spot(function(n)
		return math.random(1, n)
	end))
end

local function title()
	state.mode = "title"
	state.paused = false
	state.view = "orbit"
	state.driver = "autopilot"
	cargo.clear()
	for _, job in jobs do
		drop_job_beacon(job)
	end
	jobs = {}
	show_depot(false)
	to_spawn()
	wander()
end

local function begin_shift()
	state.mode = "shift"
	state.paused = false
	state.view = "chase"
	state.driver = if rules("autopilot") then "autopilot" else "player"
	state.pilot = nil
	cargo.clear()
	for _, job in jobs do
		drop_job_beacon(job)
	end
	jobs = {}
	score, delivered, lost_count = 0, 0, 0
	left = rules("shift_seconds")
	hurry = false
	hurry_at = math.min(HURRY, left / 4)
	audio.snapshot("default", 0.5)
	to_spawn()
	show_depot(true)
	audio.play("assets/sfx/whistle.wav", nil, 0.9, "ui")
	say(`Shift starts! Load parcels at the depot on {town.depot_street}.`, 4)
end

local function end_shift()
	state.mode = "results"
	state.driver = "parked"
	state.view = "orbit"
	state.pilot = nil
	hurry = false
	audio.snapshot("default", 1.0)
	audio.play("assets/sfx/bell.wav", nil, 1.0, "ui")
	show_depot(false)
	for _, job in jobs do
		drop_job_beacon(job)
	end
	best = math.max(best, score)
end

-- New parcels at the depot: addresses away from it and from each other.
local function load_parcels()
	local want = math.min(rules("parcels") :: number, cargo.CAPACITY - cargo.aboard())
	local taken: { [Entity]: boolean } = {}
	for _, job in jobs do
		if not job.done then
			taken[job.address.entity] = true
		end
	end
	local loaded = 0
	for _ = 1, want * 20 do
		if loaded >= want or #town.addresses == 0 then
			break
		end
		local address = town.addresses[math.random(1, #town.addresses)]
		if not taken[address.entity] and util.flat_distance(address.spot, town.bay) > FAR_FROM_DEPOT then
			taken[address.entity] = true
			local job: Job = { id = next_job, address = address, loaded = time.now(), fell = false, done = false }
			next_job += 1
			if cargo.load(job.id) then
				job.beacon = beacon(address.spot, "materials/beacon.material.json")
				table.insert(jobs, job)
				loaded += 1
			end
		end
	end
	if loaded > 0 then
		audio.play("assets/sfx/load.wav", nil, 1.0, "ui")
		say(`{loaded} parcels loaded. Follow the yellow beacons!`, 3)
		show_depot(false)
	end
end

local function job_by_id(id: number): Job?
	for _, job in jobs do
		if job.id == id then
			return job
		end
	end
	return nil
end

local function deliver(job: Job)
	cargo.unload(job.id)
	job.done = true
	drop_job_beacon(job)
	local quick = math.max(0, 75 - (time.now() - job.loaded)) * 2
	local careful = if job.fell then 0 else 50
	local points = 100 + math.floor(quick) + careful
	score += points
	delivered += 1
	audio.play("assets/sfx/deliver.wav", nil, 1.0, "ui")
	local extra = if careful > 0 then " (handled with care)" else ""
	say(`+{points}  Delivered to {job.address.label}{extra}`, 3, "good")
end

-- The nearest job still to deliver whose parcel is aboard.
local function nearest_job(at: vector): Job?
	local best_job, best_d = nil, math.huge
	for _, job in jobs do
		if not job.done and cargo.position(job.id) == nil then
			local d = util.flat_distance(at, job.address.spot)
			if d < best_d then
				best_job, best_d = job, d
			end
		end
	end
	return best_job
end

-- The shift's work for one step.
local function work(dt: number)
	left -= dt
	if left <= 0 then
		left = 0
		end_shift()
		return
	end
	if not hurry and left < hurry_at then
		hurry = true
		audio.snapshot("hurry", 2.0)
		say(`{math.ceil(hurry_at)} seconds left!`, 2, "warn")
	end
	local at, rotation = util.world_pose(truck)
	local speed = state.speed

	-- Knocks throw parcels off; the truck picks them up again.
	local fell = cargo.knock(state.jolt, function()
		return math.random()
	end)
	if fell > 0 then
		for _, p in cargo.parcels do
			if p.body then
				local job = job_by_id(p.job)
				if job then
					job.fell = true
				end
			end
		end
		audio.play("assets/sfx/thud.wav", at, 1.0, "sfx")
		say(if fell == 1 then "A parcel fell off! Stop next to it to pick it up." else `{fell} parcels fell off! Stop next to them.`, 3, "warn")
	end
	local picked, lost = cargo.update(speed)
	if #picked > 0 then
		audio.play("assets/sfx/load.wav", nil, 0.8, "ui")
		say("Got it back.", 2, "good")
	end
	for _, id in lost do
		local job = job_by_id(id)
		if job then
			job.done = true
			drop_job_beacon(job)
			lost_count += 1
			score = math.max(0, score - 50)
			audio.play("assets/sfx/lost.wav", nil, 1.0, "ui")
			say(`-50  A parcel for {job.address.label} was lost.`, 3, "bad")
		end
	end

	-- Deliveries.
	if math.abs(speed) < DELIVER_SPEED then
		for _, job in jobs do
			if not job.done and cargo.position(job.id) == nil and util.flat_distance(at, job.address.spot) < DELIVER_RADIUS then
				deliver(job)
			end
		end
	end

	-- The depot: an empty bed (and nothing on the road) loads up.
	local outstanding = false
	for _, job in jobs do
		if not job.done then
			outstanding = true
		end
	end
	if not outstanding then
		show_depot(true)
		if util.flat_distance(at, town.bay) < BAY_RADIUS and math.abs(speed) < 1.5 then
			load_parcels()
		end
	end

	-- The autopilot, when it works the shift: to the nearest parcel's
	-- address, to a parcel on the road, or to the depot.
	if state.driver == "autopilot" then
		local target: vector? = nil
		local arrive = 3.0
		for _, p in cargo.parcels do
			if p.body then
				target = cargo.position(p.job)
				arrive = 2.0
			end
		end
		if not target then
			local job = nearest_job(at)
			if job then
				target = job.address.spot
			elseif not outstanding then
				target = town.bay
				arrive = 2.5
			end
		end
		if target then
			local pilot = state.pilot
			if not pilot or pilot.arrived and util.flat_distance(pilot.path[#pilot.path], target) > 1 or util.flat_distance(pilot.path[#pilot.path], target) > 4 then
				pilot_to(target, arrive)
			end
		end
	end
	local _ = rotation
end

input.on_pressed("start", function()
	if not ready or building then
		return
	end
	if state.mode == "title" then
		begin_shift()
	elseif state.mode == "results" then
		-- A new town: regenerate with the next seed (at the end of this
		-- frame), read it, then start the shift in it.
		building = true
		state.town_seed += 1
		proc.regenerate("town", state.town_seed)
		next_frame()
		town.refresh()
		route.prepare()
		building = false
		begin_shift()
	end
end)

input.on_pressed("pause", function()
	if state.mode ~= "shift" then
		return
	end
	state.paused = not state.paused
	time.set_scale(if state.paused then 0 else 1)
	audio.snapshot(if state.paused then "paused" else (if hurry then "hurry" else "default"), 0.3)
end)

time.on_step(function(dt)
	if not ready or building then
		return
	end
	if state.mode == "shift" then
		work(dt)
	elseif state.mode == "title" then
		local pilot = state.pilot
		if not pilot or pilot.arrived then
			wander()
		end
	end
end)

-- The HUD, every frame.
local HELP = "W/S drive · A/D steer · Space handbrake · H horn · R reset · C camera · P pause"

local function job_lines(at: vector, heading: vector): string
	local lines = { `On board: {cargo.aboard()}/{cargo.CAPACITY}` }
	local target = nearest_job(at)
	for _, job in jobs do
		if not job.done then
			local where = cargo.position(job.id)
			local spot = where or job.address.spot
			local d = util.flat_distance(at, spot)
			local arrow = util.arrow(util.turn(heading, vector.create(spot.x - at.x, 0, spot.z - at.z)))
			local mark = if job == target then "▶ " else "   "
			local label = if where then "Fallen parcel!" else job.address.label
			table.insert(lines, `{mark}{label}  {math.floor(d)} m {arrow}`)
		end
	end
	if #lines == 1 then
		local d = util.flat_distance(at, town.bay)
		local arrow = util.arrow(util.turn(heading, vector.create(town.bay.x - at.x, 0, town.bay.z - at.z)))
		table.insert(lines, `▶ Depot, {town.depot_street}  {math.floor(d)} m {arrow}`)
	end
	return table.concat(lines, "\n")
end

time.on_frame(function(_dt)
	local at, rotation = util.world_pose(truck)
	local heading = util.heading(rotation)
	set_daylight(hour_now())
	hud.message:set("HudText", "color", if state.mode == "shift" and not state.paused then message_tone else WHITE)
	hud.status:set("HudText", "color", if state.mode == "shift" and hurry then TONES.warn else WHITE)
	if not ready then
		ui.set_text(hud.message, "TOWN COURIER\n\nReading the town...")
		return
	elseif state.mode == "title" then
		ui.set_text(hud.status, "")
		ui.set_text(hud.jobs, "")
		ui.set_text(hud.speed, "")
		ui.set_text(hud.street, "")
		ui.set_text(hud.help, "Enter to start · W/S drive · A/D steer · Space handbrake")
		local lines = { "TOWN COURIER", "", "Deliver every parcel before the shift ends.", "Press Enter to start a shift." }
		if best > 0 then
			table.insert(lines, "")
			table.insert(lines, `Best score: {util.thousands(best)}`)
		end
		ui.set_text(hud.message, table.concat(lines, "\n"))
		return
	elseif state.mode == "results" then
		ui.set_text(hud.status, "")
		ui.set_text(hud.jobs, "")
		ui.set_text(hud.speed, "")
		ui.set_text(hud.street, "")
		ui.set_text(hud.help, "Enter for a new town")
		local lines = {
			"SHIFT OVER",
			"",
			`Parcels delivered: {delivered}`,
			`Parcels lost: {lost_count}`,
			`Score: {util.thousands(score)}`,
			`Best: {util.thousands(best)}`,
			"",
			if building then "Building a new town..." else "Press Enter for a new town.",
		}
		ui.set_text(hud.message, table.concat(lines, "\n"))
		return
	end
	local hour = hour_now()
	local clock = string.format("%02d:%02d", math.floor(hour), math.floor((hour % 1) * 60))
	ui.set_text(hud.status, `{clock}   Shift ends in {util.clock(left)}\nScore {util.thousands(score)}   Delivered {delivered}`)
	ui.set_text(hud.jobs, job_lines(at, heading))
	ui.set_text(hud.street, town.street_at(at))
	local gear = if state.gear < 0 then "R" elseif state.gear == 0 then "N" else `D{state.gear}`
	ui.set_text(hud.speed, `{math.floor(math.abs(state.speed) * 3.6 + 0.5)} km/h  {gear}`)
	ui.set_text(hud.help, if state.driver == "autopilot" then "Autopilot" else HELP)
	if state.paused then
		ui.set_text(hud.message, "PAUSED\nPress P to carry on")
	elseif time.now() < message_until then
		ui.set_text(hud.message, message)
	else
		ui.set_text(hud.message, "")
	end
end)

cargo.init(truck)
-- Reading the town and laying out its streets takes several steps.
task.wait()
town.refresh()
route.prepare()
ready = true
title()
