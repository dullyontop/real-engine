-- Coins: tagged parts placed from a seed, collected on touch.
local CollectionService = game:GetService("CollectionService")
local Players = game:GetService("Players")

local Shared = require(game:GetService("ReplicatedStorage"):WaitForChild("Shared"))

local Coins = {}

local rng = Random.new(Shared.SEED)

local function place(coin)
	local angle = rng:NextNumber(0, math.pi * 2)
	local r = rng:NextNumber(6, Shared.PLAZA_RADIUS)
	coin.CFrame = CFrame.new(math.cos(angle) * r, 2, math.sin(angle) * r)
end

-- `onCollect(player, coin)` runs once per pickup; the debounce attribute
-- keeps a coin from counting twice while several parts touch it.
function Coins.spawn(folder, onCollect)
	for i = 1, Shared.COIN_COUNT do
		local coin = Instance.new("Part")
		coin.Name = "Coin" .. i
		coin.Shape = Enum.PartType.Cylinder
		coin.Size = Vector3.new(0.4, 2, 2)
		coin.Anchored = true
		coin.CanCollide = false
		coin.Color = Color3.fromRGB(250, 210, 50)
		coin.Material = Enum.Material.Neon
		coin:SetAttribute("Taken", false)
		place(coin)
		CollectionService:AddTag(coin, Shared.COIN_TAG)
		coin.Parent = folder

		coin.Touched:Connect(function(hit)
			if coin:GetAttribute("Taken") then
				return
			end
			local character = hit:FindFirstAncestorOfClass("Model")
			local player = character and Players:GetPlayerFromCharacter(character)
			if not player then
				return
			end
			coin:SetAttribute("Taken", true)
			coin.Transparency = 1
			onCollect(player, coin)
			task.delay(Shared.COIN_RESPAWN, function()
				place(coin)
				coin.Transparency = 0
				coin:SetAttribute("Taken", false)
			end)
		end)
	end
end

function Coins.count()
	return #CollectionService:GetTagged(Shared.COIN_TAG)
end

return Coins
