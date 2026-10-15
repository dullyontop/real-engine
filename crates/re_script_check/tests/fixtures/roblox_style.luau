-- Roblox-style API surface, for the gap report. It compiles, but the native
-- API does not provide these names.
local Players = game:GetService("Players")
local RunService = game:GetService("RunService")

Players.PlayerAdded:Connect(function(player)
	local part = Instance.new("Part")
	part.Anchored = true
	part.Position = Vector3.new(0, 10, 0)
	part.Parent = workspace
	print(player.Name, Enum.KeyCode.Space)
end)

RunService.Heartbeat:Connect(function(dt)
	local mod = require(script.Parent.Shared)
	mod.tick(dt)
end)
