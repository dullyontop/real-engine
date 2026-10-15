-- Records every RunService event and render-step binding of the client for
-- the frame-order test. Each line is "<frame> <what>".
local RunService = game:GetService("RunService")
local frame = 0
local function log(what)
	print(frame .. " " .. what)
end

local camera = Enum.RenderPriority.Camera.Value
-- Bound out of order: the priority decides, not the bind order.
RunService:BindToRenderStep("after camera", camera + 1, function()
	log("bind " .. (camera + 1))
end)
RunService:BindToRenderStep("before camera", camera - 1, function()
	-- The lowest priority runs first in a frame.
	frame += 1
	log("bind " .. (camera - 1))
end)
RunService:BindToRenderStep("camera", camera, function()
	log("bind " .. camera)
end)
-- Bound and then removed: never runs.
RunService:BindToRenderStep("gone", Enum.RenderPriority.First.Value, function()
	log("gone")
end)
RunService:UnbindFromRenderStep("gone")

RunService.PreRender:Connect(function()
	log("PreRender")
end)
RunService.RenderStepped:Connect(function()
	log("RenderStepped")
end)
RunService.PreAnimation:Connect(function()
	log("PreAnimation")
end)
RunService.PreSimulation:Connect(function()
	log("PreSimulation")
end)
RunService.Stepped:Connect(function()
	log("Stepped")
end)
RunService.PostSimulation:Connect(function()
	log("PostSimulation")
end)
RunService.Heartbeat:Connect(function()
	log("Heartbeat")
end)
task.spawn(function()
	while true do
		task.wait()
		log("task.wait")
	end
end)
