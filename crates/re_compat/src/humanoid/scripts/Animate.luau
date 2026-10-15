-- The engine's default character animation script: plays idle, walk, run,
-- jump, fall, climb and sit clips on the character's Animator as the
-- Humanoid's state changes. Animation ids come from the description the
-- Humanoid last applied, and otherwise from the built-in clips for its rig.
-- A project that ships its own StarterCharacterScripts.Animate replaces it.

local character = script.Parent
local humanoid = character:WaitForChild("Humanoid")
local animator = humanoid:WaitForChild("Animator")

local rig = if humanoid.RigType == Enum.HumanoidRigType.R6 then "r6" else "r15"
local applied = humanoid:FindFirstChild("HumanoidDescription")

local function id(kind: string, field: string?): string
	local n = if applied and field then applied[field] else 0
	if n ~= 0 then
		return "rbxassetid://" .. n
	end
	return "builtin:" .. rig .. "_" .. kind
end

local tracks = {}
local function load(kind: string, field: string?, priority: Enum.AnimationPriority)
	local a = Instance.new("Animation")
	a.Name = kind
	a.AnimationId = id(kind, field)
	local t = animator:LoadAnimation(a)
	t.Priority = priority
	tracks[kind] = t
end
load("idle", "IdleAnimation", Enum.AnimationPriority.Idle)
load("walk", "WalkAnimation", Enum.AnimationPriority.Movement)
load("run", "RunAnimation", Enum.AnimationPriority.Movement)
load("jump", "JumpAnimation", Enum.AnimationPriority.Movement)
load("fall", "FallAnimation", Enum.AnimationPriority.Movement)
load("climb", "ClimbAnimation", Enum.AnimationPriority.Movement)
load("sit", nil, Enum.AnimationPriority.Movement)

local current: string? = nil
local function play(kind: string, fade: number, speed: number?)
	if current ~= kind then
		if current then
			tracks[current]:Stop(fade)
		end
		current = kind
		tracks[kind]:Play(fade)
	end
	if speed then
		tracks[kind]:AdjustSpeed(speed)
	end
end

tracks.idle:Play(0)

humanoid.Running:Connect(function(speed: number)
	if speed > 0.5 then
		local walk = humanoid.WalkSpeed
		if speed > 20 and walk > 20 then
			play("run", 0.2, speed / 24)
		else
			play("walk", 0.2, speed / 16)
		end
	else
		play("idle", 0.2)
	end
end)
humanoid.Jumping:Connect(function(active: boolean)
	if active then
		play("jump", 0.1)
	end
end)
humanoid.FreeFalling:Connect(function(active: boolean)
	if active then
		play("fall", 0.2)
	end
end)
humanoid.Seated:Connect(function(active: boolean)
	if active then
		play("sit", 0.3)
	end
end)
humanoid.Died:Connect(function()
	for _, t in tracks do
		t:Stop(0.1)
	end
	current = nil
end)
