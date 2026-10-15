--!strict
-- The game rules: gems are collected by touching them, the HUD counts them,
-- collecting all of them wins; R restarts, G grows a new island under you.
world.define_component("Collectible", { value = { type = "i32", default = 1 } })

local hud = script.entity :: Entity
local collected = 0
local won = false
local seed = 1

local function remaining(): number
	return #world.query("Collectible")
end

local function show()
	local left = remaining()
	if left == 0 and collected > 0 then
		won = true
		ui.set_text(hud, `All {collected} gems collected! Press R to play again`)
	else
		ui.set_text(hud, `Gems: {collected} / {collected + left}`)
	end
end

physics.on_contact(function(a, b, info)
	-- A gem touched twice in one step is gone by its second contact.
	if not info.sensor or won or not a.alive or not b.alive then
		return
	end
	local gem = if a:has("Collectible") then a elseif b:has("Collectible") then b else nil
	local other = if gem == a then b else a
	if gem == nil or other.id ~= "hero" then
		return
	end
	collected += gem:get("Collectible", "value")
	gem:destroy()
	show()
end)

input.on_pressed("restart", function()
	world.restart()
end)

input.on_pressed("regenerate", function()
	-- After a win the game is over until R: a new island would hold gems
	-- nobody can collect.
	if won then
		return
	end
	seed += 1
	proc.regenerate("island", seed)
	-- The new island is made at the end of this frame: count it a little
	-- later (task.wait() alone could resume in this frame's own step).
	task.delay(0.1, show)
end)

-- The island generates before scripts start; count once it is there.
show()
