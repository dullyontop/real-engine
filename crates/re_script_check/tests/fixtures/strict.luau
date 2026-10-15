--!strict
local function count(items: { string }): number
	local n = 0
	for _ in items do
		n += 1
	end
	return n
end

local total: number = count({ "a", "b" })
task.spawn(function()
	local dt: number = task.wait(0.5)
	print(total, dt, time.now())
end)
