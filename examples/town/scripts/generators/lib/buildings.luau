--!strict
-- The town's buildings as flat-shaded geometry with vertex colours (lib/meshb).
-- Each is built in the builder's frame: the origin at the middle of its front
-- wall on the ground, +Z out of the front towards the street, +X to the
-- right seen from the street. The body spans x in [-w/2, w/2], z in [-d, 0].
local meshb = require("./meshb")

local buildings = {}

local v = vector.create
local STOREY = 3.0 -- metres from floor to floor
local GLASS = vector.create(0.032, 0.05, 0.08) -- linear colours
local FRAME = vector.create(0.85, 0.85, 0.8)
local STEP = vector.create(0.45, 0.45, 0.42)

export type House = {
	w: number,
	d: number,
	storeys: number,
	wall: vector,
	roof: vector,
	door: vector,
	-- The door's offset from the middle of the front, in metres.
	door_x: number,
	chimney: boolean,
}

-- A window (frame, then glass a little proud of it) on a wall facing +Z at
-- depth z, centred at x, with its sill at y.
local function window(b: meshb.Builder, x: number, y: number, z: number, w: number, h: number)
	meshb.panel(b, FRAME, x - w / 2 - 0.1, y - 0.1, x + w / 2 + 0.1, y + h + 0.1, z + 0.02)
	meshb.panel(b, GLASS, x - w / 2, y, x + w / 2, y + h, z + 0.035)
end

-- Windows across a facade of width `span` facing +Z at depth z: as many
-- slots as fit, skipping the one at `skip_x` on the ground floor.
local function facade(b: meshb.Builder, span: number, z: number, storeys: number, skip_x: number?)
	local slots = math.max(1, math.floor(span / 2.6))
	local pitch = span / slots
	for s = 0, storeys - 1 do
		for i = 0, slots - 1 do
			local x = -span / 2 + pitch * (i + 0.5)
			if not (s == 0 and skip_x and math.abs(x - skip_x) < pitch * 0.6) then
				window(b, x, s * STOREY + 0.9, z, 1.1, 1.35)
			end
		end
	end
end

-- Builds the walls' windows on all four sides: each side gets its own frame
-- facing out of it.
local function all_windows(b: meshb.Builder, w: number, d: number, storeys: number, door_x: number?)
	local origin, cos, sin = b.origin, b.cos, b.sin
	local yaw = math.atan2(sin, cos)
	-- Front.
	facade(b, w - 1.0, 0, storeys, door_x)
	-- Back.
	meshb.frame(b, origin + meshb.turn(b, v(0, 0, -d)), yaw + math.pi)
	facade(b, w - 1.0, 0, storeys, nil)
	-- Right and left sides, when deep enough for a window.
	b.cos, b.sin = cos, sin
	b.origin = origin
	if d >= 8 then
		meshb.frame(b, origin + meshb.turn(b, v(w / 2, 0, -d / 2)), yaw + math.pi / 2)
		facade(b, d - 3.0, 0, storeys, nil)
		b.cos, b.sin, b.origin = cos, sin, origin
		meshb.frame(b, origin + meshb.turn(b, v(-w / 2, 0, -d / 2)), yaw - math.pi / 2)
		facade(b, d - 3.0, 0, storeys, nil)
	end
	b.cos, b.sin, b.origin = cos, sin, origin
end

-- A house with a pitched roof whose ridge runs along the street.
function buildings.house(b: meshb.Builder, h: House)
	local w, d = h.w, h.d
	local height = h.storeys * STOREY
	meshb.box(b, h.wall, v(-w / 2, 0, -d), v(w / 2, height, 0))
	-- The roof: two slopes over 0.4 m eaves, and the gable ends.
	local o, side = 0.4, 0.3
	local tan = 0.75 -- about 37 degrees
	local rise = d / 2 * tan
	local eave = height - o * tan
	local ridge = height + rise
	local x0, x1 = -w / 2 - side, w / 2 + side
	meshb.face(b, h.roof, v(x0, eave, o), v(x1, eave, o), v(x1, ridge, -d / 2), v(x0, ridge, -d / 2))
	meshb.face(b, h.roof, v(x1, eave, -d - o), v(x0, eave, -d - o), v(x0, ridge, -d / 2), v(x1, ridge, -d / 2))
	meshb.face(b, h.wall, v(w / 2, height, 0), v(w / 2, height, -d), v(w / 2, ridge, -d / 2))
	meshb.face(b, h.wall, v(-w / 2, height, -d), v(-w / 2, height, 0), v(-w / 2, ridge, -d / 2))
	if h.chimney then
		local cx = w / 2 - 1.4
		meshb.box(b, h.roof * 0.8 + v(0.05, 0.03, 0.02), v(cx - 0.4, height, -d * 0.7 - 0.4), v(cx + 0.4, ridge + 0.6, -d * 0.7 + 0.4))
	end
	-- The door, its step, and the windows.
	meshb.panel(b, FRAME, h.door_x - 0.65, 0, h.door_x + 0.65, 2.35, 0.02)
	meshb.panel(b, h.door, h.door_x - 0.5, 0.15, h.door_x + 0.5, 2.2, 0.035)
	meshb.box(b, STEP, v(h.door_x - 0.8, 0, 0), v(h.door_x + 0.8, 0.15, 0.6))
	all_windows(b, w, d, h.storeys, h.door_x)
	return height
end

export type Shop = {
	w: number,
	d: number,
	storeys: number,
	wall: vector,
	trim: vector,
	awning: vector,
	door: vector,
}

-- A shop: a flat roof behind a cornice, a wide window and an awning on the
-- ground floor, and flats above.
function buildings.shop(b: meshb.Builder, s: Shop)
	local w, d = s.w, s.d
	local height = s.storeys * STOREY + 0.6
	meshb.box(b, s.wall, v(-w / 2, 0, -d), v(w / 2, height, 0))
	meshb.box(b, s.trim, v(-w / 2 - 0.15, height, -d - 0.15), v(w / 2 + 0.15, height + 0.35, 0.15))
	-- The shop front: a band of trim, the window and the door beside it.
	meshb.panel(b, s.trim, -w / 2 + 0.3, 0, w / 2 - 0.3, 3.2, 0.02)
	local door_x = w / 2 - 1.4
	meshb.panel(b, GLASS, -w / 2 + 0.7, 0.6, door_x - 0.9, 2.6, 0.04)
	meshb.panel(b, s.door, door_x - 0.55, 0, door_x + 0.55, 2.4, 0.04)
	-- The awning slopes down towards the street, coloured on both faces.
	local ax0, ax1 = -w / 2 + 0.4, w / 2 - 0.4
	meshb.face(b, s.awning, v(ax0, 2.75, 1.5), v(ax1, 2.75, 1.5), v(ax1, 3.25, 0.02), v(ax0, 3.25, 0.02))
	meshb.face(b, s.awning * 0.6, v(ax1, 2.75, 1.5), v(ax0, 2.75, 1.5), v(ax0, 3.25, 0.02), v(ax1, 3.25, 0.02))
	-- Flats above.
	local origin = b.origin
	meshb.frame(b, origin + meshb.turn(b, v(0, STOREY + 0.6, 0)), math.atan2(b.sin, b.cos))
	all_windows(b, w, d, s.storeys - 1, nil)
	b.origin = origin
	return height
end

export type Depot = { w: number, d: number, h: number }

-- The depot: a warehouse with a roller door, a loading dock and a sign.
function buildings.depot(b: meshb.Builder, s: Depot)
	local w, d, h = s.w, s.d, s.h
	local wall = v(0.09, 0.13, 0.2)
	local roof = v(0.05, 0.055, 0.06)
	meshb.box(b, wall, v(-w / 2, 0, -d), v(w / 2, h, 0))
	local rise = 1.6
	meshb.face(b, roof, v(-w / 2 - 0.3, h, 0.3), v(w / 2 + 0.3, h, 0.3), v(w / 2 + 0.3, h + rise, -d / 2), v(-w / 2 - 0.3, h + rise, -d / 2))
	meshb.face(b, roof, v(w / 2 + 0.3, h, -d - 0.3), v(-w / 2 - 0.3, h, -d - 0.3), v(-w / 2 - 0.3, h + rise, -d / 2), v(w / 2 + 0.3, h + rise, -d / 2))
	meshb.face(b, wall, v(w / 2, h, 0), v(w / 2, h, -d), v(w / 2, h + rise, -d / 2))
	meshb.face(b, wall, v(-w / 2, h, -d), v(-w / 2, h, 0), v(-w / 2, h + rise, -d / 2))
	-- The roller door with hazard stripes over it, and the dock in front.
	meshb.panel(b, v(0.55, 0.55, 0.5), -3.5, 0, 3.5, 4.6, 0.03)
	for i = 0, 6 do
		local x = -3.5 + i
		local c = if i % 2 == 0 then v(0.8, 0.55, 0.02) else v(0.02, 0.02, 0.02)
		meshb.panel(b, c, x, 4.6, x + 1, 5.0, 0.03)
	end
	meshb.box(b, v(0.35, 0.35, 0.33), v(-5.5, 0, 0), v(5.5, 0.9, 2.2))
	-- The sign: an orange board along the front.
	meshb.panel(b, v(0.85, 0.25, 0.02), -w / 2 + 1, h - 1.6, w / 2 - 1, h - 0.4, 0.04)
	-- A personnel door and windows.
	meshb.panel(b, v(0.3, 0.3, 0.3), w / 2 - 3, 0, w / 2 - 2, 2.2, 0.03)
	for i = 0, 2 do
		local x = -w / 2 + 2.5 + i * 2.2
		window(b, x, 1.2, 0, 1.4, 1.2)
	end
end

-- A pallet stack: boxes on a pallet, `size` metres square.
function buildings.pallet(b: meshb.Builder, size: number, height: number)
	meshb.box(b, v(0.35, 0.22, 0.1), v(-size / 2, 0, -size / 2), v(size / 2, 0.15, size / 2))
	meshb.box(b, v(0.55, 0.38, 0.2), v(-size / 2 + 0.05, 0.15, -size / 2 + 0.05), v(size / 2 - 0.05, height, size / 2 - 0.05))
end

-- A park bench facing +Z: slats on two legs and a back rest.
function buildings.bench(b: meshb.Builder)
	local wood = v(0.32, 0.18, 0.08)
	local iron = v(0.03, 0.03, 0.03)
	meshb.box(b, iron, v(-0.8, 0, -0.25), v(-0.7, 0.42, 0.25))
	meshb.box(b, iron, v(0.7, 0, -0.25), v(0.8, 0.42, 0.25))
	meshb.box(b, wood, v(-0.9, 0.42, -0.25), v(0.9, 0.5, 0.25), true)
	meshb.box(b, wood, v(-0.9, 0.55, -0.32), v(0.9, 0.95, -0.25), true)
end

-- A fountain: an eight-sided basin, `r` metres across the flats, with a
-- column in the middle. The water is a separate mesh (it has its own
-- material); `water_ring` gives its outline.
function buildings.fountain(b: meshb.Builder, r: number)
	local stone = v(0.5, 0.48, 0.44)
	local inner = r - 0.35
	local top = 0.6
	local function ring(radius: number, y: number, i: number): vector
		local a = (i + 0.5) * math.pi / 4
		local k = radius / math.cos(math.pi / 8)
		return v(math.cos(a) * k, y, math.sin(a) * k)
	end
	for i = 0, 7 do
		local a0, a1 = ring(r, 0, i), ring(r, 0, i + 1)
		local b0, b1 = ring(r, top, i), ring(r, top, i + 1)
		local c0, c1 = ring(inner, top, i), ring(inner, top, i + 1)
		local d0, d1 = ring(inner, 0.2, i), ring(inner, 0.2, i + 1)
		-- Outside wall, rim, inside wall (facing in).
		meshb.face(b, stone, a1, a0, b0, b1)
		meshb.face(b, stone * 1.1, b1, b0, c0, c1)
		meshb.face(b, stone * 0.8, c1, c0, d0, d1)
	end
	meshb.box(b, stone, v(-0.4, 0, -0.4), v(0.4, 2.2, 0.4))
	meshb.box(b, stone * 1.1, v(-0.9, 2.2, -0.9), v(0.9, 2.45, 0.9))
end

return buildings
