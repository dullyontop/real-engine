-- Calls the physics members no other test reaches, once each, with a
-- check of what each gives back. Run by tests/physics.rs in two halves:
-- `setup` before some frames, `check` after.
local function near(a, b, eps)
	return math.abs(a - b) <= (eps or 1e-3)
end
local function check(ok, what)
	if not ok then
		error("member check failed: " .. what, 2)
	end
end

local M = {}

function M.setup()
	local PS = game:GetService("PhysicsService")
	-- The deprecated group API.
	PS:CreateCollisionGroup("Old")
	PS:RenameCollisionGroup("Old", "Renamed")
	local id = PS:GetCollisionGroupId("Renamed")
	check(type(id) == "number" and id > 0, "GetCollisionGroupId")
	check(PS:GetCollisionGroupName(id) == "Renamed", "GetCollisionGroupName")
	PS:CreateCollisionGroup("Gone")
	PS:RemoveCollisionGroup("Gone")
	check(not PS:IsCollisionGroupRegistered("Gone"), "RemoveCollisionGroup")

	local floor = Instance.new("Part")
	floor.Name = "Floor"
	floor.Anchored = true
	floor.Size = Vector3.new(200, 1, 200)
	floor.Position = Vector3.new(0, -0.5, 0)
	floor.Parent = workspace

	-- Part members.
	local a = Instance.new("Part")
	a.Name = "A"
	a.Size = Vector3.new(2, 2, 2)
	a.Position = Vector3.new(0, 50, 0)
	a.Orientation = Vector3.new(0, 90, 0)
	check(near(a.Orientation.Y, 90), "Orientation")
	a.CanTouch = false
	check(a.CanTouch == false, "CanTouch")
	a.RootPriority = 5
	check(a.RootPriority == 5, "RootPriority")
	-- BrickColor: the brick colour nearest to Color, and setting it sets Color.
	a.Color = Color3.fromRGB(200, 40, 30)
	check(a.BrickColor == BrickColor.new("Bright red"), "BrickColor read")
	a.BrickColor = BrickColor.new("Pastel Blue")
	check(a.Color == Color3.fromRGB(128, 187, 219), "BrickColor write")
	check(not pcall(function()
		a.BrickColor = Color3.new(1, 0, 0)
	end), "BrickColor takes a BrickColor")
	a.Parent = workspace
	PS:SetPartCollisionGroup(a, "Renamed")
	check(a.CollisionGroup == "Renamed", "SetPartCollisionGroup")
	local pp = a.CurrentPhysicalProperties
	check(near(a.Mass, pp.Density * 8, 1e-3), "Mass and CurrentPhysicalProperties")

	local b = Instance.new("Part")
	b.Name = "B"
	b.Size = Vector3.new(2, 2, 2)
	b.Position = Vector3.new(4, 50, 0)
	b.Massless = true
	check(b.Massless, "Massless")
	b.Parent = workspace
	local weld = Instance.new("WeldConstraint")
	weld.Part0 = a
	weld.Part1 = b
	weld.Parent = a
	check(#a:GetJoints() == 1, "GetJoints")
	-- A massless part adds no mass: the centre of mass stays in A.
	check((a.AssemblyCenterOfMass - a.Position).Magnitude < 1e-3, "AssemblyCenterOfMass")
	check(a:GetRootPart() == a, "RootPriority picks the root")

	local c = Instance.new("Part")
	c.Name = "C"
	c.Size = Vector3.new(2, 2, 2)
	c.Position = Vector3.new(-20, 1000, 0)
	c.Parent = workspace
	c.Velocity = Vector3.new(0, 0, 10)
	check(near(c.AssemblyLinearVelocity.Z, 10), "Velocity")
	c.RotVelocity = Vector3.new(0, 2, 0)
	check(near(c.AssemblyAngularVelocity.Y, 2), "RotVelocity")
	check(near(c.Velocity.Z, 10) and near(c.RotVelocity.Y, 2), "Velocity reads")
	-- At a point 1 stud along X a spin about Y adds -2 along Z.
	local v = c:GetVelocityAtPosition(c.Position + Vector3.new(1, 0, 0))
	check(near(v.Z, 8, 1e-2), "GetVelocityAtPosition")
	-- Applied at the next step: checked after.
	c:ApplyImpulseAtPosition(Vector3.new(0, 0, c.AssemblyMass), c.Position)

	local d = Instance.new("Part")
	d.Name = "D"
	d.Position = Vector3.new(30, 50, 0)
	d.Parent = workspace
	local e = Instance.new("Part")
	e.Name = "E"
	e.Position = Vector3.new(30, 52, 0)
	e.Parent = workspace
	local w2 = Instance.new("Weld")
	w2.Part0 = d
	w2.Part1 = e
	w2.C0 = CFrame.new(0, 2, 0)
	w2.Parent = d
	d:BreakJoints()
	check(#d:GetJoints() == 0, "BreakJoints")

	-- Model.
	local m = Instance.new("Model")
	m.Name = "M"
	local p1 = Instance.new("Part")
	p1.Anchored = true
	p1.Size = Vector3.new(4, 1, 2)
	p1.Position = Vector3.new(60, 10, 0)
	p1.Parent = m
	m.PrimaryPart = p1
	m.Parent = workspace
	check((m:GetExtentsSize() - Vector3.new(4, 1, 2)).Magnitude < 1e-3, "GetExtentsSize")
	check((m:GetPrimaryPartCFrame().Position - p1.Position).Magnitude < 1e-3, "GetPrimaryPartCFrame")

	-- Spawns.
	local s = Instance.new("SpawnLocation")
	s.Anchored = true
	s.Neutral = false
	check(s.Neutral == false, "SpawnLocation.Neutral")
	s.Position = Vector3.new(-60, 0.5, 0)
	s.Parent = workspace

	-- Attachments.
	local holder = Instance.new("Part")
	holder.Name = "Holder"
	holder.Anchored = true
	holder.CFrame = CFrame.new(0, 20, 40) * CFrame.Angles(0, math.rad(90), 0)
	holder.Parent = workspace
	local at = Instance.new("Attachment")
	at.Visible = true
	check(at.Visible, "Attachment.Visible")
	at.Orientation = Vector3.new(0, 0, 90)
	check(near(at.Orientation.Z, 90), "Attachment.Orientation")
	at.Parent = holder
	check((at.SecondaryAxis - at.CFrame.UpVector).Magnitude < 1e-3, "SecondaryAxis")
	check((at.WorldCFrame.Position - holder.Position).Magnitude < 1e-3, "WorldCFrame")
	check(near(at.WorldOrientation.Y, 90, 1e-2), "WorldOrientation")
	check((at.WorldSecondaryAxis - at.WorldCFrame.UpVector).Magnitude < 1e-3, "WorldSecondaryAxis")

	-- Constraints: a ball socket with limits.
	local function attach(part, pos)
		local x = Instance.new("Attachment")
		x.Position = pos or Vector3.zero
		x.Parent = part
		return x
	end
	local anchor = Instance.new("Part")
	anchor.Name = "Anchor"
	anchor.Anchored = true
	anchor.Position = Vector3.new(0, 30, -40)
	anchor.Parent = workspace
	local bob = Instance.new("Part")
	bob.Name = "Bob"
	bob.Size = Vector3.new(1, 1, 1)
	bob.Position = Vector3.new(4, 30, -40)
	bob.Parent = workspace
	local bs = Instance.new("BallSocketConstraint")
	bs.Attachment0 = attach(anchor)
	bs.Attachment1 = attach(bob, Vector3.new(-4, 0, 0))
	bs.LimitsEnabled = true
	bs.UpperAngle = 30
	bs.TwistLimitsEnabled = true
	bs.TwistLowerAngle = -10
	bs.TwistUpperAngle = 10
	bs.Visible = true
	check(bs.Visible and bs.UpperAngle == 30 and bs.TwistLowerAngle == -10, "BallSocket members")
	bs.Parent = anchor

	-- A servo hinge turning a paddle to 90 degrees, and a limited hinge.
	local hub = Instance.new("Part")
	hub.Name = "Hub"
	hub.Anchored = true
	hub.Size = Vector3.new(1, 1, 1)
	hub.Position = Vector3.new(40, 30, -40)
	hub.Parent = workspace
	-- The pivot is 1.5 studs off the hub, so the paddle clears it at every
	-- angle: a paddle pressed into the hub breaks free at a frame that
	-- differs between platforms.
	local paddle = Instance.new("Part")
	paddle.Name = "Paddle"
	paddle.Size = Vector3.new(1, 1, 4)
	paddle.Position = Vector3.new(40, 30, -36.5)
	paddle.Parent = workspace
	local h = Instance.new("HingeConstraint")
	h.Attachment0 = attach(hub, Vector3.new(0, 0, 1.5))
	h.Attachment1 = attach(paddle, Vector3.new(0, 0, -2))
	h.ActuatorType = Enum.ActuatorType.Servo
	h.AngularSpeed = 4
	h.ServoMaxTorque = 1e7
	h.TargetAngle = 90
	h.LimitsEnabled = true
	h.LowerAngle = -120
	h.UpperAngle = 120
	h.Parent = hub

	-- Movers relative to their attachment.
	local sled = Instance.new("Part")
	sled.Name = "Sled"
	sled.Size = Vector3.new(2, 1, 2)
	sled.CFrame = CFrame.new(-40, 0.5, -40) * CFrame.Angles(0, math.rad(90), 0)
	sled.Parent = workspace
	local lv = Instance.new("LinearVelocity")
	lv.Attachment0 = attach(sled)
	lv.RelativeTo = Enum.ActuatorRelativeTo.Attachment0
	lv.VelocityConstraintMode = Enum.VelocityConstraintMode.Line
	lv.LineDirection = Vector3.new(1, 0, 0)
	lv.LineVelocity = 5
	lv.ForceLimitsEnabled = false
	lv.Parent = sled

	local spinner = Instance.new("Part")
	spinner.Name = "Spinner"
	spinner.Position = Vector3.new(-40, 1000, 40)
	spinner.Parent = workspace
	local av = Instance.new("AngularVelocity")
	av.Attachment0 = attach(spinner)
	av.RelativeTo = Enum.ActuatorRelativeTo.World
	av.AngularVelocity = Vector3.new(0, 3, 0)
	av.MaxTorque = 1e9
	av.Parent = spinner

	local pusher = Instance.new("Part")
	pusher.Name = "Pusher"
	pusher.Position = Vector3.new(80, 0.5, 40)
	pusher.Parent = workspace
	local vf = Instance.new("VectorForce")
	vf.Attachment0 = attach(pusher)
	vf.RelativeTo = Enum.ActuatorRelativeTo.World
	vf.Force = Vector3.new(0, pusher.AssemblyMass * workspace.Gravity, 0)
	vf.Parent = pusher

	-- Align movers, capped and rigid.
	local follower = Instance.new("Part")
	follower.Name = "Follower"
	follower.Position = Vector3.new(80, 20, -40)
	follower.Parent = workspace
	local ap = Instance.new("AlignPosition")
	ap.Mode = Enum.PositionAlignmentMode.OneAttachment
	ap.Attachment0 = attach(follower)
	ap.Position = Vector3.new(80, 20, -40)
	ap.RigidityEnabled = false
	ap.MaxVelocity = 50
	ap.MaxForce = 1e9
	ap.Parent = follower
	local ao = Instance.new("AlignOrientation")
	ao.Mode = Enum.OrientationAlignmentMode.OneAttachment
	ao.Attachment0 = follower:FindFirstChildOfClass("Attachment")
	ao.CFrame = CFrame.Angles(0, math.rad(45), 0)
	ao.RigidityEnabled = true
	ao.MaxAngularVelocity = 100
	ao.Parent = follower
end

function M.check()
	local paddle = workspace.Paddle
	local hinge = workspace.Hub:FindFirstChildOfClass("HingeConstraint")
	check(near(hinge.CurrentAngle, 90, 3), "TargetAngle reached: " .. hinge.CurrentAngle)
	-- The sled runs along its own X, which faces world -Z.
	local v = workspace.Sled.AssemblyLinearVelocity
	check(near(v.Z, -5, 0.3) and near(v.X, 0, 0.3), "LineVelocity relative to its attachment: " .. tostring(v))
	check(near(workspace.Spinner.AssemblyAngularVelocity.Y, 3, 0.1), "AngularVelocity")
	-- The vector force holds the pusher's weight: it rests, unmoved.
	check(near(workspace.Pusher.Position.Y, 0.5, 0.05), "VectorForce")
	check((workspace.Follower.Position - Vector3.new(80, 20, -40)).Magnitude < 0.2, "AlignPosition")
	local _, y = workspace.Follower.CFrame:ToOrientation()
	check(near(math.deg(y), 45, 1), "AlignOrientation")
	-- The ball socket's limit keeps the bob within 30 degrees of level.
	local bob = workspace.Bob.Position - workspace.Anchor.Position
	local below = math.deg(math.asin(-bob.Unit.Y))
	check(below <= 32, "BallSocket UpperAngle: " .. below)
	-- A part that cannot touch is not reported by touches.
	check(#workspace.A:GetTouchingParts() == 0, "CanTouch false")
	check(near(workspace.C.AssemblyLinearVelocity.Z, 11, 1e-2), "ApplyImpulseAtPosition: " .. tostring(workspace.C.AssemblyLinearVelocity) .. " " .. tostring(workspace.C.Position))
	print("members ok")
end

return M
