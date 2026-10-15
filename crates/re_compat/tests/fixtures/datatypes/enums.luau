local item = Enum.Material.Plastic
assert(typeof(Enum) == "Enums")
assert(typeof(Enum.Material) == "Enum")
assert(typeof(item) == "EnumItem")
assert(item.Name == "Plastic" and type(item.Value) == "number")
assert(item.EnumType == Enum.Material)
assert(rawequal(Enum.Material.Plastic, Enum.Material.Plastic))
assert(rawequal(Enum.Material, Enum.Material))
assert(item ~= Enum.Material.Neon)
assert(item:IsA("Material") and not item:IsA("KeyCode"))
assert(tostring(item) == "Enum.Material.Plastic")
assert(tostring(Enum.Material) == "Material")
assert(tostring(Enum) == "Enums")
-- Items work as table keys.
local map = { [Enum.KeyCode.E] = "use" }
assert(map[Enum.KeyCode.E] == "use")
local found = false
for _, i in Enum.KeyCode:GetEnumItems() do
	assert(i.EnumType == Enum.KeyCode)
	if i == Enum.KeyCode.E then
		found = true
	end
end
assert(found)
assert(Enum.Material:FromName("Neon") == Enum.Material.Neon)
assert(Enum.Material:FromName("Nope") == nil)
assert(Enum.Material:FromValue(Enum.Material.Neon.Value) == Enum.Material.Neon)
assert(#Enum:GetEnums() > 100)
assert(Enum.RunContext.Client.Value == 2)
local ok, err = pcall(function()
	return (Enum.Material :: any).Nope
end)
assert(not ok and string.find(tostring(err), "RE9001", 1, true))
ok = pcall(function()
	return (Enum :: any).NotAnEnum
end)
assert(not ok)
ok = pcall(function()
	(Enum :: any).Material = 1
end)
assert(not ok)
