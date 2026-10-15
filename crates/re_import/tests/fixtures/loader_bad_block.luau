-- A synthetic loader whose building block holds a statement the importer
-- does not read: the import must stop and write nothing.
local function new(kind, label, where)
	local made = Instance.new(kind)
	made.Name = label
	made.Parent = where
	return made
end

local ok = pcall(function()
	local f = new("Folder", "Stuff", game:GetService("ReplicatedStorage"))
	f:SetAttribute("Count", 3)
end)
