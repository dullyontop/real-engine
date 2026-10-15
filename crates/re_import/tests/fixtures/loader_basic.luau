-- A synthetic loader for the importer tests, written for this repository.
-- It builds a small tree with helper functions, the way generated Studio
-- command-bar loaders do, and is read statically: none of it runs.
local Players = game:GetService("Players")
print("loading the garden kit")

local function at(names)
	local node = game
	for index, name in ipairs(names) do
		if index == 1 then
			node = game:GetService(name)
		else
			local found = node:FindFirstChild(name)
			if not found then
				found = Instance.new("Folder")
				found.Name = name
				found.Parent = node
			end
			node = found
		end
	end
	return node
end

local function wipe(where, label)
	for _, kid in where:GetChildren() do
		if kid.Name == label then
			kid:Destroy()
		end
	end
end

local function new(kind, label, where)
	local made = Instance.new(kind)
	made.Name = label
	made.Parent = where
	return made
end

local done = pcall(function()
	wipe(at({ "ReplicatedStorage" }), "Garden")
	do
		local g = new("Folder", "Garden", at({ "ReplicatedStorage" }))
		do
			local plants = new("Folder", "Plants", g)
			local rose = new("ModuleScript", "Rose", plants)
			rose.Source = "return { petals = 5 }\n"
			local twin = new("ModuleScript", "Rose", plants)
			twin.Source = [[
-- a second module with the same name
return { petals = 6 }
]]
		end
		local odd = new("ModuleScript", "Seeds.v2", g)
		odd.Source = [==[
local s = "]]"
return s
]==]
	end
	wipe(at({ "ServerScriptService" }), "Gardener")
	do
		local s = new("Script", "Gardener", at({ "ServerScriptService" }))
		s.Source = "print(\"tending\\tthe garden\")\n"
	end
	wipe(at({ "StarterPlayer", "StarterPlayerScripts" }), "Watering")
	do
		local l = new("LocalScript", "Watering", at({ "StarterPlayer", "StarterPlayerScripts" }))
		l.Source = "local can = 3\nreturn can\n"
	end
end)

if done then
	print("garden kit loaded")
end
error("this final statement would fail if the loader ran")
