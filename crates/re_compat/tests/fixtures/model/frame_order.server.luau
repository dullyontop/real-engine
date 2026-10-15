-- Records every RunService event of the server for the frame-order test.
local RunService = game:GetService("RunService")
local frame = 0
local function log(what)
	print(frame .. " " .. what)
end

RunService.RenderStepped:Connect(function()
	log("RenderStepped")
end)
RunService.PreAnimation:Connect(function()
	frame += 1
	log("PreAnimation")
end)
RunService.PreSimulation:Connect(function()
	log("PreSimulation")
end)
RunService.Stepped:Connect(function(time, dt)
	log(string.format("Stepped %.4f %.4f", time, dt))
end)
RunService.PostSimulation:Connect(function()
	log("PostSimulation")
end)
RunService.Heartbeat:Connect(function()
	log("Heartbeat")
end)
