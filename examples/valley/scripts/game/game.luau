--!strict
-- The HUD: what the player can do now, the car's speed while driving, and
-- when there is no room to get out.
local rig = require("./lib/rig")

local hud = script.entity :: Entity
local hero = world.get("hero") :: Entity
local car = world.get("car") :: Entity

time.on_frame(function(_dt)
	if rig.driving and time.now() < rig.no_room + 2 then
		ui.set_text(hud, "No room to get out here")
	elseif rig.driving then
		local v = physics.vehicle(car)
		ui.set_text(hud, string.format("%d km/h  gear %d   E: get out", math.round(math.abs(v.speed) * 3.6), v.gear))
	else
		local d = vector.magnitude(car:get("Transform", "translation") - hero:get("Transform", "translation"))
		ui.set_text(hud, if d < 4.5 then "E: drive" else "WASD: walk  Space: jump")
	end
end)
