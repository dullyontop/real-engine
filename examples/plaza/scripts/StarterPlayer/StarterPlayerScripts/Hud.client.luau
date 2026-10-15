-- The coin counter and the shop's answer, built in code.
local Players = game:GetService("Players")
local ReplicatedStorage = game:GetService("ReplicatedStorage")

local Shared = require(ReplicatedStorage:WaitForChild("Shared"))
local remotes = Shared.remotes()

local player = Players.LocalPlayer

local gui = Instance.new("ScreenGui")
gui.Name = "Hud"
gui.ResetOnSpawn = false

local panel = Instance.new("Frame")
panel.Name = "Panel"
panel.Size = UDim2.fromOffset(180, 48)
panel.Position = UDim2.new(0, 16, 0, 16)
panel.BackgroundColor3 = Color3.fromRGB(20, 24, 32)
panel.BackgroundTransparency = 0.25
panel.Parent = gui

local counter = Instance.new("TextLabel")
counter.Name = "Coins"
counter.Size = UDim2.fromScale(1, 1)
counter.BackgroundTransparency = 1
counter.Font = Enum.Font.GothamBold
counter.TextSize = 24
counter.TextColor3 = Color3.fromRGB(250, 210, 50)
counter.Text = "Coins: 0"
counter.Parent = panel

local notice = Instance.new("TextLabel")
notice.Name = "Notice"
notice.Size = UDim2.fromOffset(320, 32)
notice.AnchorPoint = Vector2.new(0.5, 0)
notice.Position = UDim2.new(0.5, 0, 0, 16)
notice.BackgroundTransparency = 1
notice.Font = Enum.Font.Gotham
notice.TextSize = 20
notice.TextColor3 = Color3.new(1, 1, 1)
notice.Text = ""
notice.Parent = gui

gui.Parent = player:WaitForChild("PlayerGui")

remotes.CoinsChanged.OnClientEvent:Connect(function(value)
	counter.Text = "Coins: " .. value
end)

-- Each notice clears after two seconds unless a newer one replaced it.
local shown = 0
remotes.ShopResult.OnClientEvent:Connect(function(item, ok, why)
	notice.Text = if ok
		then "Bought " .. item .. "!"
		elseif why == "owned" then "You already have " .. item
		else "Not enough coins for " .. item
	shown += 1
	local mine = shown
	task.wait(2)
	if shown == mine then
		notice.Text = ""
	end
end)
