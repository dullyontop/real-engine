-- Requires by relative path and by alias resolve to the same module.
local vm = require("./lib/vecmath")
local vm2 = require("@lib/vecmath")
assert(vm == vm2)

local mid = vm.lerp(vector.create(0, 0, 0), vector.create(2, 4, 6), 0.5)
print(vm.flat(mid))
