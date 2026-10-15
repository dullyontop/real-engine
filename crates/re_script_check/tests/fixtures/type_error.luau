--!strict
-- Deliberate type error against engine.d.luau: world.get takes a string id.
local e = world.get(42)
print(e)
