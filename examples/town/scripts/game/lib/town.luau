--!strict
-- What the game knows about the generated town, read from the markers the
-- project.town node kind leaves in it (scripts/generators/town.luau): every
-- address with the kerbside spot its parcels go to, the depot's loading bay,
-- the truck's spawn point, and the junctions with their street names.
-- `town.refresh()` reads them again after a regeneration.
--
-- Reading a town of a few hundred markers takes a few milliseconds, and one
-- script callback may run for 4 ms of wall time ([scripts] cpu_budget_ms), so
-- `refresh` waits for the next step after every BATCH markers: call it from a
-- script thread that may wait (a script's top level, an input callback, or
-- task.spawn). Its world poses are cached for the refresh: the markers share
-- their parents, and each step up a hierarchy costs a few engine calls.
local util = require("./util")

local BATCH = 48

local town = {}

export type Address = {
	entity: Entity,
	-- "Bakery, 3 High Street" or "12 Oak Street".
	label: string,
	street: string,
	-- Where the truck stops to deliver, on the road at the kerb.
	spot: vector,
}

export type Junction = {
	id: number,
	row: number,
	col: number,
	at: vector,
	row_street: string,
	col_street: string,
	-- Neighbouring junction ids along the streets.
	links: { number },
}

town.addresses = {} :: { Address }
town.junctions = {} :: { [number]: Junction }
town.bay = vector.zero
town.bay_heading = vector.create(0, 0, 1)
town.spawn = vector.zero
town.spawn_rotation = nil :: Quat?
town.depot_street = ""
-- Bumped by each refresh, so others can tell the town changed.
town.version = 0

type Pose = { at: vector, rotation: Quat, scale: vector }

-- World poses of the entities read so far in one refresh.
local poses: { [Entity]: Pose } = {}

local function pose(e: Entity): Pose
	local known = poses[e]
	if known then
		return known
	end
	local at: vector = e:get("Transform", "translation")
	local rotation: Quat = e:get("Transform", "rotation")
	local scale: vector = e:get("Transform", "scale")
	local parent = e:parent()
	if parent and parent:has("Transform") then
		local p = pose(parent)
		at = p.at + p.rotation * (at * p.scale)
		rotation = p.rotation * rotation
		scale = p.scale * scale
	end
	local made = { at = at, rotation = rotation, scale = scale }
	poses[e] = made
	return made
end

-- A point in the space of `e`'s parent (where a generated marker's fields
-- are given), in world space.
local function parent_point(e: Entity, p: vector): vector
	local parent = e:parent()
	if not parent or not parent:has("Transform") then
		return p
	end
	local frame = pose(parent)
	return frame.at + frame.rotation * (p * frame.scale)
end

function town.refresh()
	poses = {}
	local addresses: { Address } = {}
	for i, e in world.query("Address") do
		if i % BATCH == 0 then
			task.wait()
		end
		if not e.alive then
			continue
		end
		local street: string = e:get("Address", "street")
		local number: number = e:get("Address", "number")
		local name: string = e:get("Address", "name")
		local label = `{number} {street}`
		if name ~= "" then
			label = `{name}, {label}`
		end
		table.insert(addresses, {
			entity = e,
			label = label,
			street = street,
			spot = parent_point(e, e:get("Address", "spot")),
		})
	end
	-- Query order is entity order; sort by label so the list does not
	-- depend on it.
	table.sort(addresses, function(a: Address, b: Address)
		return a.label < b.label
	end)
	town.addresses = addresses

	local junctions: { [number]: Junction } = {}
	for _, e in world.query("Junction") do
		local row: number = e:get("Junction", "row")
		local col: number = e:get("Junction", "col")
		local id = row * 100 + col
		junctions[id] = {
			id = id,
			row = row,
			col = col,
			at = pose(e).at,
			row_street = e:get("Junction", "row_street"),
			col_street = e:get("Junction", "col_street"),
			links = {},
		}
	end
	for id, j in junctions do
		for _, other in { id - 1, id + 1, id - 100, id + 100 } do
			if junctions[other] then
				table.insert(j.links, other)
			end
		end
		table.sort(j.links)
	end
	town.junctions = junctions

	for _, e in world.query("Depot") do
		local p = pose(e)
		town.bay = p.at
		town.bay_heading = util.heading(p.rotation)
		town.depot_street = e:get("Depot", "street")
	end
	for _, e in world.query("SpawnPoint") do
		local p = pose(e)
		town.spawn = p.at
		town.spawn_rotation = p.rotation
	end
	poses = {}
	town.version += 1
end

-- The name of the street nearest `p`: the one along the junction-to-junction
-- stretch closest to it.
function town.street_at(p: vector): string
	local best, name = math.huge, ""
	for id, j in town.junctions do
		for _, other in j.links do
			if other > id then
				local k = town.junctions[other]
				local ab = vector.create(k.at.x - j.at.x, 0, k.at.z - j.at.z)
				local ap = vector.create(p.x - j.at.x, 0, p.z - j.at.z)
				local len2 = vector.dot(ab, ab)
				local t = if len2 > 0 then math.clamp(vector.dot(ap, ab) / len2, 0, 1) else 0
				local d = vector.magnitude(ap - ab * t)
				if d < best then
					best = d
					name = if other - id == 1 then j.row_street else j.col_street
				end
			end
		end
	end
	return name
end

-- The junction nearest `p`.
function town.nearest_junction(p: vector): Junction?
	local best, found = math.huge, nil
	for _, j in town.junctions do
		local d = util.flat_distance(j.at, p)
		if d < best or (d == best and found and j.id < found.id) then
			best, found = d, j
		end
	end
	return found
end

return town
