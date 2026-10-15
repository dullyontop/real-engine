-- Server half of the member-call fixture (docs/M2_PLAN.md, definition of
-- done item 9): reaches the implemented members no other test calls.
-- `cargo xtask members` checks the union of every test's calls.

-- Reads each property, and writes the value back when it is writable.
local function reach(obj, writable, readOnly)
	for _, name in writable do
		local value = obj[name]
		obj[name] = value
	end
	for _, name in readOnly do
		local _ = obj[name]
	end
end

local function noop() end
local function touch(signal)
	signal:Connect(noop):Disconnect()
end

reach(Instance.new("AlignOrientation", workspace), {"MaxTorque", "Responsiveness", "Enabled"}, {})
reach(Instance.new("AlignPosition", workspace), {"ApplyAtCenterOfMass"}, {})
reach(Instance.new("Atmosphere", game:GetService("Lighting")), {"Color", "Density", "Haze"}, {})
reach(Instance.new("CornerWedgePart", workspace), {"Rotation"}, {})
reach(Instance.new("Beam", workspace), {"Enabled"}, {})
reach(Instance.new("ColorCorrectionEffect", game:GetService("Lighting")), {"Contrast", "TintColor"}, {})
reach(game:GetService("Debris"), {"MaxItems"}, {})
reach(Instance.new("ForceField", workspace), {"Visible"}, {})
reach(Instance.new("Highlight", workspace), {"Adornee", "DepthMode", "Enabled", "OutlineColor", "OutlineTransparency"}, {})
reach(game:GetService("HttpService"), {}, {"HttpEnabled"})
reach(Instance.new("HumanoidDescription"), {"ClimbAnimation", "DepthScale", "FallAnimation", "HeadScale", "IdleAnimation", "JumpAnimation", "LeftArmColor", "LeftLegColor", "RightArmColor", "RightLegColor", "RunAnimation", "WalkAnimation", "WidthScale"}, {})
reach(Instance.new("IKControl", workspace), {"Offset", "Pole", "Priority"}, {})
reach(Instance.new("ManualWeld", workspace), {"Enabled"}, {"Active"})
reach(Instance.new("KeyframeSequence"), {"Priority"}, {})
reach(game:GetService("Lighting"), {"Ambient", "Brightness", "FogColor", "GeographicLatitude", "GlobalShadows", "OutdoorAmbient"}, {})
reach(Instance.new("ParticleEmitter", workspace), {"Drag", "EmissionDirection", "Enabled", "RotSpeed", "Rotation", "Shape", "ShapeInOut", "ShapeStyle", "TimeScale"}, {})
reach(game:GetService("Players"), {}, {"MaxPlayers"})
reach(Instance.new("Pose"), {"EasingDirection", "EasingStyle", "Weight"}, {})
reach(Instance.new("BloomEffect", game:GetService("Lighting")), {"Enabled"}, {})
reach(Instance.new("ProximityPrompt", workspace), {"AutoLocalize", "ClickablePrompt", "Enabled", "Exclusivity", "GamepadKeyCode", "KeyboardKeyCode", "Style", "UIOffset"}, {})
reach(game:GetService("ProximityPromptService"), {"Enabled", "MaxPromptsVisible"}, {})
reach(Instance.new("RodConstraint", workspace), {"Length"}, {"CurrentDistance"})
reach(Instance.new("Sky", game:GetService("Lighting")), {"CelestialBodiesShown"}, {})
reach(Instance.new("Sound", workspace), {"RollOffMaxDistance", "RollOffMinDistance", "RollOffMode"}, {})
reach(game:GetService("SoundService"), {"AmbientReverb", "DistanceFactor", "DopplerScale", "RespectFilteringEnabled", "RolloffScale"}, {})
reach(game:GetService("StarterPlayer"), {"AutoJumpEnabled", "CameraMaxZoomDistance", "CameraMinZoomDistance", "CameraMode", "CharacterJumpHeight", "CharacterJumpPower", "CharacterMaxSlopeAngle", "CharacterUseJumpPower", "CharacterWalkSpeed", "EnableMouseLockOption", "HealthDisplayDistance", "LoadCharacterAppearance", "NameDisplayDistance"}, {})
reach(Instance.new("SurfaceLight", workspace), {"Angle", "Face", "Range"}, {})
reach(Instance.new("TeleportOptions"), {"ReservedServerAccessCode", "ServerInstanceId", "ShouldReserveServer"}, {})
reach(Instance.new("TextChatCommand", workspace), {"AutocompleteVisible", "Enabled", "SecondaryAlias"}, {})
reach(game:GetService("TextChatService"), {"ChatVersion", "CreateDefaultCommands", "CreateDefaultTextChannels"}, {})
reach(Instance.new("Trail", workspace), {"Enabled", "MaxLength", "MinLength", "WidthScale"}, {})
reach(Instance.new("WeldConstraint", workspace), {"Enabled"}, {"Active"})

-- Instance methods, including the deprecated lowercase spellings.
local holder = Instance.new("Folder", workspace)
for i = 1, 2 do
	Instance.new("Folder", holder).Name = "Child" .. i
end
assert(holder:isA("Folder"))
assert(#holder:getChildren() == 2)
assert(holder:findFirstChild("Child1") ~= nil)
local copy = holder:clone()
copy.Parent = workspace
copy:ClearAllChildren()
assert(#copy:GetChildren() == 0)
copy:destroy()
holder:FindFirstChild("Child1"):Remove()
holder:FindFirstChild("Child2"):remove()
holder:Destroy()

-- Tags.
local CollectionService = game:GetService("CollectionService")
local tagged = Instance.new("Part", workspace)
CollectionService:AddTag(tagged, "MemberCheck")
assert(table.find(CollectionService:GetTags(tagged), "MemberCheck"))
assert(table.find(CollectionService:GetAllTags(), "MemberCheck"))
CollectionService:RemoveTag(tagged, "MemberCheck")
assert(#CollectionService:GetTags(tagged) == 0)

-- Prompt events, on the prompt and on the service.
local prompt = Instance.new("ProximityPrompt", tagged)
touch(prompt.PromptButtonHoldEnded)
touch(prompt.PromptHidden)
touch(prompt.PromptShown)
local prompts = game:GetService("ProximityPromptService")
touch(prompts.PromptButtonHoldBegan)
touch(prompts.PromptButtonHoldEnded)
touch(prompts.PromptHidden)
touch(prompts.PromptShown)
touch(prompts.PromptTriggerEnded)

-- Sound events.
local sound = Instance.new("Sound", workspace)
touch(sound.Paused)
touch(sound.Resumed)
touch(sound.Stopped)

assert(game:GetService("RunService"):IsEdit() == false)

-- An unreliable remote: the server fires to the client, which answers.
local unreliable = Instance.new("UnreliableRemoteEvent")
unreliable.Name = "MemberCheckUnreliable"
unreliable.Parent = game:GetService("ReplicatedStorage")
unreliable.OnServerEvent:Connect(function(player, text)
	print("unreliable from " .. player.Name .. ": " .. text)
end)
local player = game:GetService("Players"):GetPlayers()[1]
unreliable:FireClient(player, "one")
unreliable:FireAllClients("all")

-- The chat channel a player's TextSource lives in; the test adds the source.
local channels = Instance.new("Folder")
channels.Name = "TextChannels"
channels.Parent = game:GetService("TextChatService")
local general = Instance.new("TextChannel")
general.Name = "RBXGeneral"
general.Parent = channels

-- Library members.
local look = CFrame.lookAlong(Vector3.zero, Vector3.new(0, 0, -1))
assert(look.LookVector:FuzzyEq(Vector3.new(0, 0, -1)))
assert(debug.info(1, "s") ~= nil)
local thread = task.delay(60, noop)
task.cancel(thread)
print("server members ok")
