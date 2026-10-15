-- A ScriptRef script: counts how often the entity it is attached to was touched.
local touched = 0
physics.on_touch(script.entity, function(other)
	touched += 1
	script.entity:set("HudText", "text", `touched {touched}`)
end)
