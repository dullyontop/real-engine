--!strict
-- Keeps the player's progress in the save slot "progress", and stays across
-- switches. Version 1 of the data counted gold; version 2 counts coins, and
-- is what this build writes ([saves] version = 2). The project seeds a
-- version 1 slot (data/seed/saves/progress.json), so every run that keeps no
-- data begins by migrating it; a run that keeps data reads its own folder,
-- and starts a new game when the slot is empty. Each scene switched to earns
-- a coin; the progress is saved, read back and shown.
local progress = script.entity :: Entity
scene.keep(progress)

type Progress = { coins: number, visited: { string } }

save.migrate(1, function(old)
	return { coins = old.gold, visited = old.visited }
end)

local data = (save.read("progress") or { coins = 0, visited = { scene.current() } }) :: Progress
local last = scene.current()

time.on_step(function(_dt)
	local now = scene.current()
	if now ~= last then
		last = now
		data.coins += 1
		table.insert(data.visited, now)
		save.write("progress", data)
		data = save.read("progress") :: Progress
	end
	ui.set_text(progress, `coins {data.coins}, visited {table.concat(data.visited, " > ")}`)
end)
