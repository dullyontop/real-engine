--!strict
-- Drives the tour, and stays across switches. Each "next" takes one step:
-- from the title to the town, keeping the player; the HUD loaded over the
-- town; the HUD unloaded; back to the title, the player kept again.
-- "restart" restarts the run.
local director = script.entity :: Entity
scene.keep(director)

local busy = false
local hud_seen = false

-- Waits for a load on a thread of its own, which resumes in the first
-- script phase after the load applied.
local function follow(load: SceneLoad)
	busy = true
	task.spawn(function()
		load:wait()
		busy = false
		if load.error then
			warn(`{load.name} did not load: {load.error}`)
		else
			print(`{load.name} loaded`)
		end
	end)
end

input.on_pressed("next", function()
	if busy then
		return
	end
	local loaded = scene.loaded()
	if scene.current() == "title" then
		scene.keep(world.get("player") :: Entity)
		follow(scene.switch("town"))
	elseif loaded[2] == "hud" then
		-- The HUD goes at the end of this frame.
		scene.unload("hud")
		hud_seen = true
	elseif not hud_seen then
		follow(scene.load("hud"))
	else
		hud_seen = false
		follow(scene.switch("title"))
	end
end)

input.on_pressed("restart", function()
	world.restart()
end)

-- Where the game is, at every step.
time.on_step(function(_dt)
	local where = `{scene.current()}: {table.concat(scene.loaded(), ", ")}`
	ui.set_text(director, if busy then `{where} (loading)` else where)
end)
