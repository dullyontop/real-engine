local c = Color3.fromRGB(255, 128, 0)
assert(typeof(c) == "Color3")
assert(c.R == 1 and c.B == 0)
assert(c == Color3.new(1, 128 / 255, 0))
assert(Color3.new() == Color3.new(0, 0, 0))
assert(Color3.fromHex("#FF8000") == c and Color3.fromHex("ff8000") == c)
assert(c:ToHex() == "FF8000")
local h, s, v = Color3.new(0, 1, 0):ToHSV()
assert(math.abs(h - 1 / 3) < 1e-6 and s == 1 and v == 1)
local h2 = Color3.toHSV(Color3.new(0, 1, 0))
assert(h2 == h)
assert(Color3.fromHSV(0, 1, 1) == Color3.new(1, 0, 0))
assert(Color3.new(0, 0, 0):Lerp(Color3.new(1, 1, 1), 0.5) == Color3.new(0.5, 0.5, 0.5))
assert(Color3.new(2, -1, 0).R == 2) -- not clamped
assert(tostring(Color3.new(1, 0.5, 0)) == "1, 0.5, 0")

local seq = ColorSequence.new(Color3.new(1, 0, 0), Color3.new(0, 0, 1))
assert(typeof(seq) == "ColorSequence")
assert(#seq.Keypoints == 2 and seq.Keypoints[2].Value == Color3.new(0, 0, 1))
local one = ColorSequence.new(Color3.new(1, 1, 1))
assert(one.Keypoints[1].Value == one.Keypoints[2].Value)
local k = ColorSequenceKeypoint.new(0.5, Color3.new(0, 1, 0))
assert(typeof(k) == "ColorSequenceKeypoint" and k.Time == 0.5)
local three = ColorSequence.new({
	ColorSequenceKeypoint.new(0, Color3.new(1, 0, 0)),
	k,
	ColorSequenceKeypoint.new(1, Color3.new(0, 0, 1)),
})
assert(#three.Keypoints == 3 and three.Keypoints[2] == k)
assert(seq == ColorSequence.new(Color3.new(1, 0, 0), Color3.new(0, 0, 1)))
-- The keypoint rules.
assert(not pcall(ColorSequence.new, { k }))
assert(not pcall(ColorSequence.new, { ColorSequenceKeypoint.new(0.1, c), ColorSequenceKeypoint.new(1, c) }))
assert(not pcall(ColorSequence.new, { ColorSequenceKeypoint.new(0, c), ColorSequenceKeypoint.new(0.9, c) }))
assert(not pcall(Color3.fromHex, "#12"))
