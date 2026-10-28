--!strict
-- The town's buildings and the markers the game finds its way by (a Luau
-- node kind, ADR-0043 §4):
--
-- * Houses along every side of each `houses` block, facing the street,
--   numbered along it (odd on the left, even on the right). Shops on the
--   main streets, under a flat roof with an awning. Each block's buildings
--   are one mesh; each building is a child entity with a box collider and an
--   `Address` (street, number, the shop's name, and the kerbside `spot`
--   where its parcels are left).
-- * The depot in the `depot` block: a warehouse set back behind a yard, the
--   loading `bay` (a `Depot` marker) and the truck's `SpawnPoint`.
-- * A fountain in each `park` block (the trees are project.greenery's).
-- * A `Junction` marker at each junction, with its row, column and the
--   names of its two streets.
--
-- Street names, house sizes, colours and shops are drawn from the node's
-- seed. The game reads the markers' components, declared in the scene's
-- `script_components`.
local plan = require("lib/plan")
local poly = require("lib/poly")
local meshb = require("lib/meshb")
local buildings = require("lib/buildings")

local NAMES = {
	"Oak", "Elm", "Maple", "Birch", "Cedar", "Willow", "Rowan", "Hazel",
	"Linden", "Alder", "Poplar", "Laurel", "Aspen", "Juniper", "Holly", "Chestnut",
}
local SHOPS = {
	"Bakery", "Florist", "Post Office", "Pharmacy", "Bookshop", "Cafe", "Grocer",
	"Hardware Store", "Barber", "Tailor", "Toy Shop", "Cheese Shop", "Bike Shop", "Butcher",
}
local WALLS = {
	plan.rgb(236, 222, 190), plan.rgb(232, 232, 224), plan.rgb(240, 214, 150), plan.rgb(205, 125, 90),
	plan.rgb(160, 190, 212), plan.rgb(170, 192, 152), plan.rgb(226, 176, 168), plan.rgb(165, 86, 64),
	plan.rgb(194, 176, 212), plan.rgb(216, 194, 152),
}
local ROOFS = { plan.rgb(96, 64, 52), plan.rgb(150, 72, 52), plan.rgb(72, 78, 88), plan.rgb(58, 62, 62) }
local DOORS = {
	plan.rgb(122, 40, 40), plan.rgb(40, 72, 122), plan.rgb(40, 92, 62), plan.rgb(92, 62, 40),
	plan.rgb(34, 34, 38), plan.rgb(200, 160, 44),
}
local AWNINGS = { plan.rgb(196, 52, 50), plan.rgb(40, 110, 160), plan.rgb(52, 140, 82), plan.rgb(220, 160, 44) }
local TRIMS = { plan.rgb(60, 66, 74), plan.rgb(240, 236, 226), plan.rgb(110, 40, 44), plan.rgb(36, 70, 60) }

local MATERIAL = "materials/building.material.json"
local WATER = "materials/water.material.json"
local PAVEMENT = 2.5 -- the pavement's width, which the blocks stop short of

type Lot = {
	box: poly.Box,
	-- Where the front wall's middle is, on the ground, and the way it faces.
	front: vector,
	facing: vector,
	-- The kerbside spot on the road in front.
	spot: vector,
}

type Placed = {
	key: string,
	lot: Lot,
	street: number,
	side: number,
	at: number,
	shop: boolean,
}

return proc.node({
	kind = "project.town",
	version = 1,
	doc = "The town's buildings with addresses, the depot, park fountains and junction markers.",
	inputs = {
		{ name = "plan", type = "points", required = true, doc = "The junctions and blocks (project.town_plan)." },
		{ name = "blocks", type = "shapes", required = true, doc = "The blocks: the town minus its roads and pavements." },
	},
	output = "fragment",
	uses_seed = true,
	params = {
		ground = proc.param.number({ default = 6.0, doc = "The town's ground height, in metres." }),
		lot = proc.param.number({ default = 12, min = 8, max = 24, doc = "A plot's usual width along the street, in metres." }),
		setback = proc.param.number({ default = 2.5, min = 0, max = 10, doc = "Front gardens' depth, in metres." }),
	},
	eval = function(ctx, inputs, params)
		local rng = ctx.rng
		local function pick<T>(list: { T }): T
			return list[rng:int(1, #list)]
		end
		local p = plan.read(inputs.plan)
		local streets = plan.streets(p)
		local lines: { { vector } } = {}
		for i, s in streets do
			lines[i] = plan.centreline(s, 2, 9 / 2)
		end

		-- Street names: the edge streets are roads named for where they
		-- are, the middle row and column are the high street and the
		-- market avenue, the rest are trees.
		local pool = table.clone(NAMES)
		for i = #pool, 2, -1 do
			local j = rng:int(1, i)
			pool[i], pool[j] = pool[j], pool[i]
		end
		local names: { string } = {}
		for i, s in streets do
			if s.ring then
				if s.axis == "row" then
					names[i] = if s.index == 0 then "North Road" else "South Road"
				else
					names[i] = if s.index == 0 then "West Road" else "East Road"
				end
			elseif s.main then
				names[i] = if s.axis == "row" then "High Street" else "Market Avenue"
			else
				names[i] = assert(table.remove(pool), "out of street names") .. (if s.axis == "row" then " Street" else " Avenue")
			end
		end
		local shops = table.clone(SHOPS)

		-- The nearest street to a point: its index, which side of it the
		-- point is on (1 left, 2 right, along its direction), and how far
		-- along it.
		local function locate(at: vector): (number, number, number)
			local best, best_i, best_side, best_s = math.huge, 1, 1, 0
			for i, line in lines do
				local walked = 0
				for k = 1, #line - 1 do
					local a, b = line[k], line[k + 1]
					local ab = vector.create(b.x - a.x, 0, b.z - a.z)
					local ap = vector.create(at.x - a.x, 0, at.z - a.z)
					local len = vector.magnitude(ab)
					local t = if len > 0 then math.clamp(vector.dot(ap, ab) / (len * len), 0, 1) else 0
					local d = vector.magnitude(ap - ab * t)
					if d < best then
						best, best_i, best_s = d, i, walked + len * t
						best_side = if ab.x * ap.z - ab.z * ap.x > 0 then 2 else 1
					end
					walked += len
				end
			end
			return best_i, best_side, best_s
		end

		local items: { any } = {}
		local function item(t: any)
			table.insert(items, t)
		end
		local Y = vector.create(0, 1, 0)
		local function yaw_of(facing: vector): number
			return math.atan2(facing.x, facing.z)
		end

		-- Lots along a block's sides. The sides more along X than Z run to
		-- the block's corners; the others keep clear of those, so that the
		-- corner houses do not overlap.
		local function lots(ring: poly.Ring): { Lot }
			local out: { Lot } = {}
			local taken: { poly.Box } = {}
			local sides = poly.sides(ring, params.ground, math.rad(28))
			table.sort(sides, function(a, b)
				return math.abs(a.dir.x) > math.abs(b.dir.x)
			end)
			for _, side in sides do
				if side.length >= params.lot then
					local full = math.abs(side.dir.x) >= math.abs(side.dir.z)
					local clear = if full then 1.0 else params.setback + 13
					local s = clear
					while s + params.lot * 0.75 <= side.length - clear do
						local w = math.min(params.lot + (rng:next() * 2 - 1) * 2.5, side.length - clear - s)
						local d = 8 + rng:next() * 4
						local mid, dir = poly.along(side.points, s + w / 2)
						local inward = vector.create(-dir.z, 0, dir.x)
						if vector.dot(inward, side.inward) < 0 then
							inward = -inward
						end
						local front = mid + inward * params.setback
						local box: poly.Box = {
							centre = front + inward * (d / 2),
							axis = dir,
							half_w = (w - 1.6) / 2,
							half_d = d / 2,
						}
						local fits = true
						for _, c in poly.corners(box) do
							if not poly.contains(ring, c.x, c.z) then
								fits = false
							end
						end
						for _, other in taken do
							if fits and poly.overlap(box, other) then
								fits = false
							end
						end
						if fits then
							table.insert(taken, box)
							table.insert(out, {
								box = box,
								front = front,
								facing = -inward,
								spot = mid - inward * (PAVEMENT + 2.2) + Y * 0.05,
							})
						end
						s += w
					end
				end
			end
			return out
		end

		-- Match each block to its shape: the one its middle is in.
		local shapes = inputs.blocks :: { { outer: poly.Ring } }
		local function shape_of(middle: vector): poly.Ring?
			for _, shape in shapes do
				if poly.contains(shape.outer, middle.x, middle.z) then
					return shape.outer
				end
			end
			return nil
		end

		local placed: { Placed } = {}
		for _, block in plan.blocks(inputs.plan) do
			local ring = shape_of(block.middle)
			if not ring then
				ctx:log(`block {block.row}-{block.col} has no shape around its middle`)
				continue
			end
			local key = `block_{block.row}_{block.col}`
			local b = meshb.new()
			if block.use == "houses" then
				local houses: { any } = {}
				for k, lot in lots(ring) do
					local street, side, along = locate(lot.spot)
					local shop = streets[street].main and rng:next() < 0.8
					meshb.frame(b, lot.front, yaw_of(lot.facing))
					local w, d = lot.box.half_w * 2, lot.box.half_d * 2
					local height
					local name = ""
					if shop then
						height = buildings.shop(b, {
							w = w, d = d, storeys = rng:int(2, 3),
							wall = pick(WALLS), trim = pick(TRIMS), awning = pick(AWNINGS), door = pick(DOORS),
						})
						if #shops > 0 then
							name = table.remove(shops, rng:int(1, #shops)) :: string
						end
					else
						height = buildings.house(b, {
							w = w, d = d, storeys = if rng:next() < 0.6 then 2 else 1,
							wall = pick(WALLS), roof = pick(ROOFS), door = pick(DOORS),
							door_x = (rng:next() * 2 - 1) * math.max(0, w / 2 - 2.2),
							chimney = rng:next() < 0.5,
						})
					end
					local house = {
						key = `{key}/house_{k}`,
						parent = key,
						height = height,
						lot = lot,
						name = name,
					}
					table.insert(houses, house)
					table.insert(placed, { key = house.key, lot = lot, street = street, side = side, at = along, shop = shop })
				end
				if b.count > 0 then
					item({ key = key, name = `Block {block.row}-{block.col}`, mesh = meshb.finish(b, MATERIAL) })
				else
					item({ key = key, name = `Block {block.row}-{block.col}` })
				end
				for _, h in houses do
					local box = h.lot.box
					item({
						key = h.key,
						parent = h.parent,
						name = h.name,
						components = {
							Transform = {
								translation = box.centre + Y * (h.height / 2),
								rotation = quat.from_axis_angle(Y, yaw_of(h.lot.facing)),
							},
							Collider = { shape = "Box", half_extents = vector.create(box.half_w, h.height / 2, box.half_d) },
							Address = { spot = h.lot.spot, name = h.name },
						},
					})
				end
			elseif block.use == "depot" then
				-- The longest side facing along X gets the warehouse.
				local sides = poly.sides(ring, params.ground, math.rad(28))
				table.sort(sides, function(a: poly.Side, c: poly.Side)
					return a.length > c.length
				end)
				local side = sides[1]
				local mid, dir = poly.along(side.points, side.length / 2)
				local inward = vector.create(-dir.z, 0, dir.x)
				if vector.dot(inward, side.inward) < 0 then
					inward = -inward
				end
				local W, D, H, YARD = 24, 15, 7, 14
				local front = mid + inward * YARD
				local yaw = yaw_of(-inward)
				meshb.frame(b, front, yaw)
				buildings.depot(b, { w = W, d = D, h = H })
				-- The yard: concrete from the pavement to the dock.
				meshb.frame(b, mid, yaw)
				meshb.face(b, plan.rgb(150, 150, 146),
					vector.create(-W / 2 - 2, 0.03, 0), vector.create(W / 2 + 2, 0.03, 0),
					vector.create(W / 2 + 2, 0.03, -YARD), vector.create(-W / 2 - 2, 0.03, -YARD))
				local pallets = {}
				for i, x in { -W / 2 + 1.5, W / 2 - 1.5 } do
					local at = vector.create(x, 0, -YARD + 1.6)
					meshb.frame(b, mid + meshb.turn(b, at), yaw)
					buildings.pallet(b, 1.4, 1.5 + i * 0.3)
					meshb.frame(b, mid, yaw)
					table.insert(pallets, { at = mid + meshb.turn(b, at), height = 1.5 + i * 0.3 })
				end
				item({ key = "depot", name = "Depot", mesh = meshb.finish(b, MATERIAL) })
				item({
					key = "depot/warehouse",
					parent = "depot",
					name = "Warehouse",
					components = {
						Transform = {
							translation = front + inward * (D / 2) + Y * (H / 2),
							rotation = quat.from_axis_angle(Y, yaw),
						},
						Collider = { shape = "Box", half_extents = vector.create(W / 2, H / 2, D / 2) },
					},
				})
				item({
					key = "depot/dock",
					parent = "depot",
					name = "Dock",
					components = {
						Transform = {
							translation = front - inward * 1.1 + Y * 0.45,
							rotation = quat.from_axis_angle(Y, yaw),
						},
						Collider = { shape = "Box", half_extents = vector.create(5.5, 0.45, 1.1) },
					},
				})
				for i, pallet in pallets do
					item({
						key = `depot/pallet_{i}`,
						parent = "depot",
						name = "Pallets",
						components = {
							Transform = { translation = pallet.at + Y * (pallet.height / 2), rotation = quat.from_axis_angle(Y, yaw) },
							Collider = { shape = "Box", half_extents = vector.create(0.7, pallet.height / 2, 0.7) },
						},
					})
				end
				-- The bay, where the truck loads, backs onto the dock; the
				-- truck starts in the yard facing the street.
				local street = locate(mid - inward * (PAVEMENT + 2))
				item({
					key = "depot/bay",
					parent = "depot",
					name = "Loading Bay",
					components = {
						Transform = { translation = front - inward * 6 + Y * 0.05, rotation = quat.from_axis_angle(Y, yaw) },
						Depot = { street = names[street] },
					},
				})
				item({
					key = "depot/spawn",
					parent = "depot",
					name = "Truck Spawn",
					components = {
						Transform = { translation = front - inward * 8 + Y * 1.2, rotation = quat.from_axis_angle(Y, yaw) },
						SpawnPoint = {},
					},
				})
			elseif block.use == "park" then
				local middle = vector.create(block.middle.x, params.ground, block.middle.z)
				meshb.frame(b, middle, 0)
				buildings.fountain(b, 3.2)
				-- Four benches round the fountain, facing it.
				local benches = {}
				for i = 0, 3 do
					local a = i * math.pi / 2 + math.pi / 4
					local dir = vector.create(math.cos(a), 0, math.sin(a))
					local at = middle + dir * 6.5
					meshb.frame(b, at, yaw_of(-dir))
					buildings.bench(b)
					table.insert(benches, { at = at, yaw = yaw_of(-dir) })
				end
				item({ key = key, name = `Park {block.row}-{block.col}`, mesh = meshb.finish(b, MATERIAL) })
				for i, bench in benches do
					item({
						key = `{key}/bench_{i}`,
						parent = key,
						name = "Bench",
						components = {
							Transform = { translation = bench.at + Y * 0.48, rotation = quat.from_axis_angle(Y, bench.yaw) },
							Collider = { shape = "Box", half_extents = vector.create(0.9, 0.48, 0.32) },
						},
					})
				end
				local water = meshb.new()
				meshb.frame(water, middle, 0)
				local r = (3.2 - 0.35) / math.cos(math.pi / 8)
				for i = 0, 7 do
					local a0, a1 = (i + 0.5) * math.pi / 4, (i + 1.5) * math.pi / 4
					meshb.face(water, vector.one,
						vector.create(0, 0.45, 0),
						vector.create(math.cos(a1) * r, 0.45, math.sin(a1) * r),
						vector.create(math.cos(a0) * r, 0.45, math.sin(a0) * r))
				end
				item({ key = `{key}/water`, parent = key, name = "Fountain", mesh = meshb.finish(water, WATER) })
				item({
					key = `{key}/basin`,
					parent = key,
					name = "Basin",
					components = {
						Transform = { translation = middle + Y * 0.3 },
						Collider = { shape = "Box", half_extents = vector.create(3.2, 0.3, 3.2) },
					},
				})
			end
		end

		-- Number the houses along each street: odd on the left, even on the
		-- right, from the street's first junction.
		table.sort(placed, function(a: Placed, b: Placed)
			if a.street ~= b.street then
				return a.street < b.street
			elseif a.side ~= b.side then
				return a.side < b.side
			end
			return a.at < b.at
		end)
		local next_number: { [string]: number } = {}
		local by_key: { [string]: any } = {}
		for _, it in items do
			by_key[it.key] = it
		end
		for _, h in placed do
			local k = `{h.street}/{h.side}`
			local n = next_number[k] or h.side
			next_number[k] = n + 2
			local it = by_key[h.key]
			local address = it.components.Address
			address.street = names[h.street]
			address.number = n
			local label = `{n} {names[h.street]}`
			it.name = if it.name ~= "" then `{it.name}, {label}` else label
		end

		-- The junctions, for the game's route finding.
		for r = 0, p.rows - 1 do
			for c = 0, p.cols - 1 do
				local at = p.at[plan.id(r, c)]
				local row_name, col_name = names[r + 1], names[p.rows + c + 1]
				item({
					key = `junction_{r}_{c}`,
					name = `{row_name} & {col_name}`,
					components = {
						Transform = { translation = at },
						Junction = { row = r, col = c, row_street = row_name, col_street = col_name },
					},
				})
			end
		end
		return { items = items }
	end,
})
