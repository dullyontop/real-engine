--!strict
-- The engine's default player module: keyboard movement into the local
-- character's Humanoid, and the default (Custom) camera. A project that ships
-- its own StarterPlayerScripts.PlayerModule replaces it.
--
-- Controls: W A S D or the arrow keys walk, relative to the camera; Space
-- jumps (and leaves a seat). Camera: hold the right mouse button and move the
-- mouse to orbit, turn the wheel to zoom; I and O zoom too.

local Players = game:GetService("Players")
local RunService = game:GetService("RunService")
local UserInputService = game:GetService("UserInputService")

local player = Players.LocalPlayer

local MOVE_KEYS = {
	[Enum.KeyCode.W] = Vector3.new(0, 0, -1),
	[Enum.KeyCode.Up] = Vector3.new(0, 0, -1),
	[Enum.KeyCode.S] = Vector3.new(0, 0, 1),
	[Enum.KeyCode.Down] = Vector3.new(0, 0, 1),
	[Enum.KeyCode.A] = Vector3.new(-1, 0, 0),
	[Enum.KeyCode.Left] = Vector3.new(-1, 0, 0),
	[Enum.KeyCode.D] = Vector3.new(1, 0, 0),
	[Enum.KeyCode.Right] = Vector3.new(1, 0, 0),
}

-- Controls -------------------------------------------------------------------

local Controls = {}
Controls.__index = Controls

function Controls.new()
	return setmetatable({ enabled = true }, Controls)
end

function Controls:Enable(enable: boolean?)
	self.enabled = enable ~= false
end

function Controls:Disable()
	self.enabled = false
end

-- The walk direction the keys ask for, in the camera's frame.
function Controls:GetMoveVector(): Vector3
	if not self.enabled or UserInputService:GetFocusedTextBox() then
		return Vector3.zero
	end
	local v = Vector3.zero
	for key, dir in MOVE_KEYS do
		if UserInputService:IsKeyDown(key) then
			v += dir
		end
	end
	if v.Magnitude > 0 then
		v = v.Unit
	end
	return v
end

function Controls:Update()
	local character = player.Character
	local humanoid = character and character:FindFirstChildOfClass("Humanoid")
	if not humanoid then
		return
	end
	humanoid:Move(self:GetMoveVector(), true)
	if self.enabled then
		humanoid.Jump = UserInputService:IsKeyDown(Enum.KeyCode.Space)
	end
end

-- Camera ---------------------------------------------------------------------

local Camera = {}
Camera.__index = Camera

local MIN_PITCH, MAX_PITCH = math.rad(-80), math.rad(80)
local ORBIT_SPEED = math.rad(0.4) -- radians per pixel of mouse motion

function Camera.new()
	return setmetatable({ yaw = 0, pitch = math.rad(-15), zoom = 12.5, dragging = false }, Camera)
end

function Camera:Zoom(by: number)
	local lo = player.CameraMinZoomDistance
	local hi = player.CameraMaxZoomDistance
	self.zoom = math.clamp(self.zoom + by, lo, hi)
end

function Camera:Update(dt: number)
	local camera = workspace.CurrentCamera
	if not camera or camera.CameraType ~= Enum.CameraType.Custom then
		return
	end
	local character = player.Character
	local humanoid = character and character:FindFirstChildOfClass("Humanoid")
	local root = humanoid and humanoid.RootPart
	if not root then
		return
	end
	camera.CameraSubject = humanoid
	if self.dragging then
		local d = UserInputService:GetMouseDelta()
		self.yaw -= d.X * ORBIT_SPEED
		self.pitch = math.clamp(self.pitch - d.Y * ORBIT_SPEED, MIN_PITCH, MAX_PITCH)
	end
	if UserInputService:IsKeyDown(Enum.KeyCode.I) then
		self:Zoom(-20 * dt)
	elseif UserInputService:IsKeyDown(Enum.KeyCode.O) then
		self:Zoom(20 * dt)
	end
	local focus = root.Position + Vector3.new(0, 1.5, 0) + humanoid.CameraOffset
	local turn = CFrame.Angles(0, self.yaw, 0) * CFrame.Angles(self.pitch, 0, 0)
	camera.CFrame = CFrame.new(focus) * turn * CFrame.new(0, 0, self.zoom)
	camera.Focus = CFrame.new(focus)
end

-- The module -----------------------------------------------------------------

local PlayerModule = {}
PlayerModule.__index = PlayerModule

function PlayerModule.new()
	local self = setmetatable({ controls = Controls.new(), camera = Camera.new() }, PlayerModule)
	UserInputService.InputBegan:Connect(function(input, processed)
		if not processed and input.UserInputType == Enum.UserInputType.MouseButton2 then
			self.camera.dragging = true
			UserInputService.MouseBehavior = Enum.MouseBehavior.LockCurrentPosition
		end
	end)
	UserInputService.InputEnded:Connect(function(input)
		if input.UserInputType == Enum.UserInputType.MouseButton2 then
			self.camera.dragging = false
			UserInputService.MouseBehavior = Enum.MouseBehavior.Default
		end
	end)
	UserInputService.InputChanged:Connect(function(input, processed)
		if not processed and input.UserInputType == Enum.UserInputType.MouseWheel then
			self.camera:Zoom(-input.Position.Z * 2)
		end
	end)
	RunService:BindToRenderStep("ControlScript", Enum.RenderPriority.Input.Value, function()
		self.controls:Update()
	end)
	RunService:BindToRenderStep("CameraScript", Enum.RenderPriority.Camera.Value, function(dt)
		self.camera:Update(dt)
	end)
	return self
end

function PlayerModule:GetControls()
	return self.controls
end

function PlayerModule:GetCameras()
	return self.camera
end

return PlayerModule.new()
