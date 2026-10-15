-- Builds the plaza: the ground, a spinner, a door and the shop stand.
local World = {}

local function part(props)
	local p = Instance.new("Part")
	p.Anchored = true
	for k, v in props do
		p[k] = v
	end
	return p
end

function World.build(root)
	local map = Instance.new("Folder")
	map.Name = "Plaza"

	part({
		Name = "Ground",
		Size = Vector3.new(120, 2, 120),
		CFrame = CFrame.new(0, -1, 0),
		Color = Color3.fromRGB(110, 160, 90),
		Material = Enum.Material.Grass,
		Parent = map,
	})

	local spinner = part({
		Name = "Spinner",
		Size = Vector3.new(16, 1, 1),
		CFrame = CFrame.new(0, 1.5, 0),
		Color = Color3.fromRGB(230, 120, 40),
		CanCollide = false,
		Parent = map,
	})

	local door = part({
		Name = "Door",
		Size = Vector3.new(8, 10, 1),
		CFrame = CFrame.new(0, 5, -30),
		Color = Color3.fromRGB(120, 80, 50),
		Material = Enum.Material.Wood,
		Parent = map,
	})
	door:SetAttribute("Open", false)

	local stand = part({
		Name = "Shop",
		Size = Vector3.new(6, 3, 4),
		CFrame = CFrame.new(20, 1.5, 20),
		Color = Color3.fromRGB(60, 90, 200),
		Parent = map,
	})
	local prompt = Instance.new("ProximityPrompt")
	prompt.ActionText = "Buy speed"
	prompt.ObjectText = "Shop"
	prompt.HoldDuration = 0.5
	prompt.MaxActivationDistance = 12
	prompt.Parent = stand

	local coins = Instance.new("Folder")
	coins.Name = "Coins"
	coins.Parent = map

	map.Parent = root
	return { spinner = spinner, door = door, prompt = prompt, coins = coins }
end

return World
