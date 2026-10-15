-- Client half of the member-call fixture (docs/M2_PLAN.md, definition of
-- done item 9). Runs as the local player once the character exists.

local Players = game:GetService("Players")
local me = Players.LocalPlayer
local character = me.Character

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

local screen = Instance.new("ScreenGui")
screen.Parent = me:WaitForChild("PlayerGui")
local function gui(class)
	return Instance.new(class, screen)
end

local function humanoid()
	return character:FindFirstChildOfClass("Humanoid")
end

local loadedTrack
local function track()
	if not loadedTrack then
		local animation = Instance.new("Animation")
		animation.AnimationId = "rbxassetid://507766388"
		loadedTrack = humanoid():FindFirstChildOfClass("Animator"):LoadAnimation(animation)
	end
	return loadedTrack
end

reach(track(), {}, {"Animation", "WeightTarget"})
reach(gui("BillboardGui"), {"Active", "ClipsDescendants", "SizeOffset", "StudsOffsetWorldSpace", "ZIndexBehavior"}, {"AbsoluteRotation"})
reach(workspace.CurrentCamera, {}, {"NearPlaneZ"})
reach(gui("ImageButton"), {"AutoButtonColor", "Selectable", "Selected", "ImageColor3", "ImageTransparency"}, {})
reach(gui("Frame"), {"BorderColor3", "ClipsDescendants", "Interactable", "Selectable"}, {})
reach(game:GetService("GuiService"), {"AutoSelectGuiEnabled", "GuiNavigationEnabled", "SelectedObject"}, {"MenuIsOpen"})
reach(humanoid(), {"AutoRotate", "EvaluateStateMachine", "HipHeight", "JumpPower", "MaxSlopeAngle", "PlatformStand", "RequiresNeck", "WalkToPart", "WalkToPoint"}, {"FloorMaterial"})
reach(gui("ImageLabel"), {"ImageTransparency"}, {})
reach(me, {"AutoJumpEnabled", "CameraMaxZoomDistance", "CameraMinZoomDistance", "CameraMode", "CanLoadCharacterAppearance", "CharacterAppearanceId", "DisplayName", "HealthDisplayDistance", "NameDisplayDistance", "Neutral"}, {"AccountAge", "LocaleId", "MembershipType"})
reach(gui("ScrollingFrame"), {"AutomaticCanvasSize", "ClipsDescendants", "ScrollBarImageTransparency", "ScrollingDirection", "ScrollingEnabled"}, {})
reach(game:GetService("StarterGui"), {"ShowDevelopmentGui"}, {})
reach(gui("TextBox"), {"Font", "FontFace", "LineHeight", "MultiLine", "PlaceholderColor3", "PlaceholderText", "TextColor3", "TextEditable", "TextScaled", "TextSize", "TextStrokeColor3", "TextStrokeTransparency", "TextTransparency", "TextTruncate", "TextWrapped", "TextXAlignment", "TextYAlignment"}, {"TextBounds", "TextFits"})
reach(gui("TextButton"), {"FontFace", "LineHeight", "TextScaled", "TextStrokeColor3", "TextStrokeTransparency", "TextTransparency", "TextTruncate", "TextWrapped", "TextXAlignment", "TextYAlignment"}, {"TextBounds", "TextFits"})
reach(gui("TextLabel"), {"LineHeight", "TextTransparency", "TextTruncate", "TextWrapped", "TextYAlignment"}, {})
reach(gui("UIAspectRatioConstraint"), {"AspectType", "DominantAxis"}, {})
reach(gui("UIGradient"), {"Enabled", "Offset"}, {})
reach(gui("UIGridLayout"), {"FillDirection", "FillDirectionMaxCells", "StartCorner", "VerticalAlignment"}, {})
reach(gui("UIStroke"), {"ApplyStrokeMode", "Enabled", "Transparency"}, {})
reach(game:GetService("UserInputService"), {}, {"AccelerometerEnabled", "GyroscopeEnabled"})

-- The character: humanoid events and queries, the animator and a track.
local h = humanoid()
touch(h.FallingDown)
touch(h.GettingUp)
touch(h.PlatformStanding)
touch(h.Ragdoll)
assert(h:GetBodyPartR15(character:FindFirstChild("HumanoidRootPart")) ~= nil)
local animator = h:FindFirstChildOfClass("Animator")
touch(animator.AnimationPlayed)
local t = track()
touch(t.KeyframeReached)
t:Play()
t:AdjustWeight(0.5, 0)
animator:StepAnimations(1 / 60)
assert(type(h:GetPlayingAnimationTracks()) == "table")
t:Stop(0)

-- The player.
touch(me.CharacterAppearanceLoaded)
assert(me:HasAppearanceLoaded() ~= nil)
assert(type(me:GetNetworkPing()) == "number")

-- The camera and input.
local _ = workspace.CurrentCamera:GetRenderCFrame()
local UserInputService = game:GetService("UserInputService")
touch(UserInputService.GamepadConnected)
touch(UserInputService.GamepadDisconnected)
assert(type(UserInputService:GetKeysPressed()) == "table")
local ContextActionService = game:GetService("ContextActionService")
ContextActionService:BindAction("MemberCheck", noop, false, Enum.KeyCode.F)
assert(ContextActionService:GetAllBoundActionInfo().MemberCheck ~= nil)
ContextActionService:UnbindAllActions()

-- GUI events.
local button = gui("ImageButton")
touch(button.MouseButton2Down)
touch(button.MouseButton2Up)
touch(gui("Frame").MouseMoved)

-- The unreliable remote the server made.
local unreliable = game:GetService("ReplicatedStorage"):WaitForChild("MemberCheckUnreliable")
unreliable.OnClientEvent:Connect(function(text)
	print("unreliable to client: " .. text)
end)
unreliable:FireServer("back")
print("client members ok")
