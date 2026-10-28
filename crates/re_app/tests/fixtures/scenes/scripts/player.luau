--!strict
-- Walks with the move stick; kept across switches, so the player the title
-- made walks on through the town.
local SPEED = 5.0
local player = script.entity :: Entity

time.on_step(function(_dt)
	local stick = input.axis("move")
	physics.move_character(player, vector.create(stick.x, 0, -stick.y) * SPEED)
end)
