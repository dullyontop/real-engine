-- BrickColor: by number, name, colour and palette index, the named
-- constructors, the members, equality and tostring.
local blue = BrickColor.new(11)
assert(typeof(blue) == "BrickColor")
assert(blue.Number == 11 and blue.Name == "Pastel Blue")
assert(tostring(blue) == "Pastel Blue")
assert(BrickColor.new("Pastel Blue") == blue)
assert(BrickColor.new(0.502, 0.734, 0.859) == blue)
assert(BrickColor.new(Color3.new(0.502, 0.734, 0.859)) == blue)
assert(blue.Color == Color3.fromRGB(128, 187, 219))
assert(blue.r == blue.Color.R and blue.g == blue.Color.G and blue.b == blue.Color.B)
assert(blue ~= BrickColor.new(1))

-- Unknown numbers and names give Medium stone grey; nil does too.
assert(BrickColor.new(4).Name == "Medium stone grey")
assert(BrickColor.new("No such colour").Number == 194)
assert(BrickColor.new().Number == 194)
assert(BrickColor.new(21.9).Name == "Bright red")

-- A name two colours share means the palette's one.
assert(BrickColor.new("Gold").Number == 333)
assert(BrickColor.palette(70) == BrickColor.new("Gold"))
assert(BrickColor.palette(0).Name == "Earth green")
assert(BrickColor.palette(127).Name == "Really black")
assert(not pcall(BrickColor.palette, 128))
assert(not pcall(BrickColor.palette, -1))
assert(not pcall(BrickColor.palette, 1.5))
assert(not pcall(BrickColor.new, true))

-- The nearest brick colour of an arbitrary colour.
assert(BrickColor.new(Color3.new(1, 0, 0)).Name == "Really red")
assert(BrickColor.new(0, 0, 0).Name == "Really black")

-- The named constructors.
assert(BrickColor.White().Number == 1)
assert(BrickColor.Gray().Name == "Medium stone grey")
assert(BrickColor.DarkGray().Name == "Dark stone grey")
assert(BrickColor.Black().Number == 26)
assert(BrickColor.Red().Name == "Bright red")
assert(BrickColor.Yellow().Name == "Bright yellow")
assert(BrickColor.Green().Name == "Dark green")
assert(BrickColor.Blue().Name == "Bright blue")

-- random draws palette colours.
local palette = {}
for i = 0, 127 do
	palette[BrickColor.palette(i).Number] = true
end
for _ = 1, 20 do
	local c = BrickColor.random()
	assert(palette[c.Number], c.Name)
end

-- Values are immutable.
assert(not pcall(function()
	blue.Number = 1
end))
assert(not pcall(function()
	return blue.Nope
end))
