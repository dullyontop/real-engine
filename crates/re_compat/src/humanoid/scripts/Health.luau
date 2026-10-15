-- The engine's default health script: the character regains 1% of its
-- MaxHealth each second while it is alive and hurt. A project that ships its
-- own StarterCharacterScripts.Health replaces it.

local REGEN_PER_SECOND = 0.01
local STEP = 1

local humanoid = script.Parent:WaitForChild("Humanoid")

while true do
	task.wait(STEP)
	if humanoid.Health <= 0 then
		break
	end
	if humanoid.Health < humanoid.MaxHealth then
		humanoid.Health = math.min(humanoid.Health + REGEN_PER_SECOND * STEP * humanoid.MaxHealth, humanoid.MaxHealth)
	end
end
