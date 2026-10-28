--!strict
-- The parcels on the truck's bed. A loaded parcel is a box drawn as a child
-- of the truck in one of the bed's slots; the truck has one collision box
-- (an entity has one shape), so the parcels ride along as scenery. A hard
-- knock (`state.jolt`) can throw parcels off: each then becomes a dynamic
-- body of its own, flung with the truck's velocity, that the truck picks up
-- again by stopping next to it. One left on the road too long is lost.
local util = require("./util")

local cargo = {}

-- Slots on the bed, in the truck's frame (its bed floor is at y = -0.54).
local SLOTS: { vector } = {
	vector.create(0.45, -0.54, -0.2),
	vector.create(-0.45, -0.54, -0.2),
	vector.create(0.45, -0.54, -1.05),
	vector.create(-0.45, -0.54, -1.05),
	vector.create(0.45, -0.54, -1.9),
	vector.create(-0.45, -0.54, -1.9),
}
cargo.CAPACITY = #SLOTS
local SIZES: { vector } = {
	vector.create(0.7, 0.5, 0.6),
	vector.create(0.6, 0.6, 0.7),
	vector.create(0.75, 0.4, 0.5),
}
local PICKUP = 3.5 -- metres from the bed's middle
local LANDED = 1.5 -- seconds before a thrown parcel can be picked up
local LOST_AFTER = 25 -- seconds on the road

export type Parcel = {
	-- What it is for (an index into the game's jobs).
	job: number,
	size: vector,
	-- On the bed: its slot and the drawn box.
	slot: number?,
	box: Entity?,
	-- Thrown off: its body and when it fell (time.now()).
	body: Entity?,
	fell: number,
}

cargo.parcels = {} :: { Parcel }

local truck: Entity? = nil
local count = 0 -- parcels made so far, for unique ids

function cargo.init(t: Entity)
	truck = t
end

local function free_slot(): number?
	for i = 1, #SLOTS do
		local used = false
		for _, p in cargo.parcels do
			if p.slot == i then
				used = true
			end
		end
		if not used then
			return i
		end
	end
	return nil
end

local function place(p: Parcel, slot: number)
	count += 1
	p.slot = slot
	p.box = world.spawn({
		id = `parcel_{count}`,
		name = "Parcel",
		parent = truck,
		components = {
			Transform = {
				translation = SLOTS[slot] + vector.create(0, p.size.y / 2, 0),
				scale = p.size,
			},
			MeshRef = { asset = "builtin:cube" },
			MaterialRef = { asset = "materials/cardboard.material.json" },
		},
	})
end

-- Loads a parcel for `job`, if a slot is free. Returns whether it did.
function cargo.load(job: number): boolean
	local slot = free_slot()
	if not slot then
		return false
	end
	local p: Parcel = { job = job, size = SIZES[(job - 1) % #SIZES + 1], fell = 0 }
	place(p, slot)
	table.insert(cargo.parcels, p)
	return true
end

-- How many parcels are on the bed (not on the road).
function cargo.aboard(): number
	local n = 0
	for _, p in cargo.parcels do
		if p.slot then
			n += 1
		end
	end
	return n
end

local function remove(i: number)
	local p = cargo.parcels[i]
	if p.box then
		p.box:destroy()
	end
	if p.body then
		p.body:destroy()
	end
	table.remove(cargo.parcels, i)
end

-- Takes the parcel for `job` off the bed (delivered). Returns whether one
-- was there.
function cargo.unload(job: number): boolean
	for i, p in cargo.parcels do
		if p.job == job and p.slot then
			remove(i)
			return true
		end
	end
	return false
end

-- Empties the bed and the road.
function cargo.clear()
	for i = #cargo.parcels, 1, -1 do
		remove(i)
	end
end

-- Throws parcel `p` off the bed: its box goes, and a body of the same size
-- appears where it was, moving as the truck moved plus a kick.
local function throw(p: Parcel, kick: vector)
	local t = truck :: Entity
	local box = p.box :: Entity
	local at, rotation = util.world_pose(box)
	box:destroy()
	p.box = nil
	p.slot = nil
	count += 1
	local body = world.spawn({
		id = `parcel_{count}`,
		name = "Parcel",
		components = {
			Transform = { translation = at + vector.create(0, 0.3, 0), rotation = rotation, scale = p.size },
			MeshRef = { asset = "builtin:cube" },
			MaterialRef = { asset = "materials/cardboard.material.json" },
			Collider = { shape = "Box", half_extents = vector.create(0.5, 0.5, 0.5) },
			RigidBody = { kind = "Dynamic", mass = 8, friction = 0.7, restitution = 0.1 },
		},
	})
	local linear = physics.velocity(t)
	physics.set_velocity(body, linear + kick)
	p.body = body
	p.fell = time.now()
end

-- After a knock of `jolt` m/s (`state.jolt`): parcels fall off, more the
-- harder the knock, the top-most (last loaded) first. `roll` gives a number
-- in [0, 1) for each chance. Returns how many fell.
function cargo.knock(jolt: number, roll: () -> number): number
	if jolt <= 0.4 or not truck then
		return 0
	end
	local t = truck :: Entity
	local _, rotation = util.world_pose(t)
	local chance = math.clamp((jolt - 0.4) / 2.5, 0, 0.85)
	local fell = 0
	for i = #cargo.parcels, 1, -1 do
		local p = cargo.parcels[i]
		if p.slot and roll() < chance then
			local side = if (p.slot :: number) % 2 == 1 then 1 else -1
			local kick = rotation * vector.create(side * (1 + roll() * 2), 2 + roll() * 2, -1 - roll())
			throw(p, kick)
			fell += 1
		end
	end
	return fell
end

-- Picks parcels on the road back up when the truck stops next to them, and
-- loses the ones left too long. Returns the jobs picked up and the jobs lost.
function cargo.update(speed: number): ({ number }, { number })
	local picked, lost = {}, {}
	if not truck then
		return picked, lost
	end
	local t = truck :: Entity
	local at, rotation = util.world_pose(t)
	local bed = at + rotation * vector.create(0, 0, -1)
	for i = #cargo.parcels, 1, -1 do
		local p = cargo.parcels[i]
		local body = p.body
		if body then
			local where = (util.world_pose(body))
			local on_road = time.now() - p.fell
			if on_road > LANDED and math.abs(speed) < 4 and util.flat_distance(where, bed) < PICKUP then
				local slot = free_slot()
				if slot then
					body:destroy()
					p.body = nil
					place(p, slot)
					table.insert(picked, p.job)
				end
			elseif on_road > LOST_AFTER then
				table.insert(lost, p.job)
				remove(i)
			end
		end
	end
	return picked, lost
end

-- Where a parcel on the road is, for the HUD (nil while it is aboard).
function cargo.position(job: number): vector?
	for _, p in cargo.parcels do
		if p.job == job and p.body then
			return (util.world_pose(p.body))
		end
	end
	return nil
end

return cargo
