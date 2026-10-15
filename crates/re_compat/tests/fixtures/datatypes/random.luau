local a = Random.new(42)
local b = Random.new(42)
assert(typeof(a) == "Random")
for _ = 1, 20 do
	assert(a:NextNumber() == b:NextNumber())
end
for _ = 1, 200 do
	local x = a:NextNumber()
	assert(x >= 0 and x < 1)
	local y = a:NextNumber(5, 10)
	assert(y >= 5 and y < 10)
	local i = a:NextInteger(-3, 3)
	assert(i >= -3 and i <= 3 and i == math.floor(i))
	local u = a:NextUnitVector()
	assert(math.abs(u.Magnitude - 1) < 1e-5)
end
assert(not pcall(function()
	a:NextInteger(5, 1)
end))
local c = a:Clone()
assert(c:NextNumber() == a:NextNumber())
local arr = { 1, 2, 3, 4, 5, 6, 7, 8 }
a:Shuffle(arr)
local sum = 0
for _, v in arr do
	sum += v
end
assert(#arr == 8 and sum == 36)
-- Unseeded generators differ from each other but repeat across runs with the
-- same run seed (checked from Rust).
assert(Random.new():NextNumber() ~= Random.new():NextNumber())
return Random.new():NextInteger(1, 1000000000)
