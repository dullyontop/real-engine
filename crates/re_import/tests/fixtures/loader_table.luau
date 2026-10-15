-- A synthetic table-driven loader for the importer tests, written for this
-- repository: a list of script entries, then a loop that would apply them in
-- Studio. The importer applies the entries itself and never runs the loop.
local TAG = "KitManaged"
local REMOVE_OLD = true

local ENTRIES = {
	{ { "ReplicatedStorage", "Kit", "Util", "Maths" }, "ModuleScript", [=[
return { twice = function(x) return x * 2 end }
]=] },
	{ { "ReplicatedStorage", "Kit", "Util", "Text" }, "ModuleScript", [=[
return { shout = function(s) return string.upper(s) end }
]=] },
	{ { "ServerScriptService", "Boot" }, "Script", [=[
print("boot")
]=] },
	{ { "StarterPlayer", "StarterPlayerScripts", "Hud" }, "LocalScript", [=[
print("hud")
]=] },
	{ { "StarterPlayer", "StarterCharacter", "Animate" }, "LocalScript", [=[
print("animate")
]=] },
	{ { "ServerStorage", "Old", "ReplicatedStorage.Kit.Util" }, "ModuleScript", [=[
return "a dotted name is one instance"
]=] },
}

for _, entry in ENTRIES do
	local path, class, source = entry[1], entry[2], entry[3]
	local parent = game:GetService(path[1])
	for i = 2, #path - 1 do
		parent = parent:FindFirstChild(path[i]) or parent
	end
	local target = Instance.new(class)
	target.Name = path[#path]
	target.Source = source
	target.Parent = parent
end
