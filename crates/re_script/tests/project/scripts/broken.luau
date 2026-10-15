local function inner(t)
	return t.missing.field
end
local function outer()
	return inner({})
end
outer()
