--!strict
-- The HUD's banner counts the steps it has been loaded for; unloading the HUD
-- stops it with the scene.
local banner = script.entity :: Entity
local steps = 0

time.on_step(function(_dt)
	steps += 1
	ui.set_text(banner, `town, {steps} steps`)
end)
