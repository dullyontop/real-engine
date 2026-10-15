-- The plaza's server: players' coin counts, the coins, the door, the shop
-- and the test hooks.
local Players = game:GetService("Players")
local ReplicatedStorage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
local ServerStorage = game:GetService("ServerStorage")
local TweenService = game:GetService("TweenService")

local Shared = require(ReplicatedStorage:WaitForChild("Shared"))
local World = require(script.Parent:WaitForChild("World"))
local Coins = require(script.Parent:WaitForChild("Coins"))

local remotes = Instance.new("Folder")
remotes.Name = "Remotes"
for _, name in Shared.REMOTES do
	local r = Instance.new("RemoteEvent")
	r.Name = name
	r.Parent = remotes
end
remotes.Parent = ReplicatedStorage

local plaza = World.build(workspace)

-- A player's coins live in an attribute, so the client can read them too.
local function coinsOf(player)
	return player:GetAttribute("Coins") or 0
end

local function setCoins(player, value)
	player:SetAttribute("Coins", value)
	remotes.CoinsChanged:FireClient(player, value)
end

Players.PlayerAdded:Connect(function(player)
	player:SetAttribute("Coins", 0)
	player:SetAttribute("Speed", 16)
end)

Coins.spawn(plaza.coins, function(player)
	setCoins(player, coinsOf(player) + 1)
end)

-- The door slides up once someone pays for it, and stays open.
local function openDoor()
	if plaza.door:GetAttribute("Open") then
		return false
	end
	plaza.door:SetAttribute("Open", true)
	local goal = { CFrame = plaza.door.CFrame + Vector3.new(0, 9, 0) }
	TweenService:Create(plaza.door, TweenInfo.new(1.5, Enum.EasingStyle.Quad), goal):Play()
	return true
end

-- What a player can buy, and what it does.
local items = {
	speed = {
		price = Shared.SPEED_PRICE,
		apply = function(player)
			player:SetAttribute("Speed", 24)
			local humanoid = player.Character and player.Character:FindFirstChildOfClass("Humanoid")
			if humanoid then
				humanoid.WalkSpeed = 24
			end
			return true
		end,
	},
	door = { price = Shared.DOOR_PRICE, apply = function()
		return openDoor()
	end },
}

local function buy(player, item)
	local entry = items[item]
	if not entry then
		return false, "unknown item"
	end
	local coins = coinsOf(player)
	if coins < entry.price then
		remotes.ShopResult:FireClient(player, item, false)
		return false, "not enough coins"
	end
	if not entry.apply(player) then
		return false, "already bought"
	end
	setCoins(player, coins - entry.price)
	remotes.ShopResult:FireClient(player, item, true)
	return true, coins - entry.price
end

plaza.prompt.Triggered:Connect(function(player)
	buy(player, "speed")
end)

-- The spinner turns at a steady rate.
local angle = 0
RunService.Heartbeat:Connect(function(dt)
	angle = (angle + dt * math.pi / 2) % (math.pi * 2)
	plaza.spinner.CFrame = World.SPINNER * CFrame.Angles(0, angle, 0)
end)

-- Test hooks, for `play.run`'s `invoke` step.
local hooks = Instance.new("Folder")
hooks.Name = "TestHooks"

local function hook(name, fn)
	local f = Instance.new("BindableFunction")
	f.Name = name
	f.OnInvoke = fn
	f.Parent = hooks
end

local function player(name)
	return Players:FindFirstChild(name) or error("no player " .. tostring(name))
end

hook("Coins", function(name)
	return coinsOf(player(name))
end)
hook("Give", function(name, n)
	local p = player(name)
	setCoins(p, coinsOf(p) + n)
	return coinsOf(p)
end)
hook("Buy", function(name, item)
	return buy(player(name), item)
end)
hook("CoinParts", function()
	return Coins.count()
end)
hook("Position", function(name)
	local character = player(name).Character
	local root = character and character:FindFirstChild("HumanoidRootPart")
	return if root then { root.Position.X, root.Position.Y, root.Position.Z } else (if character then "no root" else "no character")
end)
hook("DoorHeight", function()
	return plaza.door.Position.Y
end)
hooks.Parent = ServerStorage

print("plaza: ready with " .. Coins.count() .. " coins")
