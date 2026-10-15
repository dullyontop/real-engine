-- Settings and helpers the server and the client share.
local Shared = {}

Shared.SEED = 2024
Shared.COIN_COUNT = 12
Shared.COIN_TAG = "Coin"
Shared.PLAZA_RADIUS = 40
Shared.COIN_RESPAWN = 8
Shared.SPEED_PRICE = 5
Shared.DOOR_PRICE = 3

-- The remotes the server makes, by name.
Shared.REMOTES = { "CoinsChanged", "ShopResult" }

function Shared.remotes()
	local folder = script.Parent:WaitForChild("Remotes")
	local out = {}
	for _, name in Shared.REMOTES do
		out[name] = folder:WaitForChild(name)
	end
	return out
end

return Shared
