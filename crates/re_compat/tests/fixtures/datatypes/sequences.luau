local r = NumberRange.new(1, 3)
assert(typeof(r) == "NumberRange" and r.Min == 1 and r.Max == 3)
assert(NumberRange.new(2).Max == 2)
assert(not pcall(NumberRange.new, 3, 1))
assert(tostring(r) == "1 3 ")
assert(r == NumberRange.new(1, 3))

local s = NumberSequence.new(0, 1)
assert(typeof(s) == "NumberSequence")
local ks = s.Keypoints
assert(#ks == 2 and ks[1].Time == 0 and ks[1].Value == 0 and ks[2].Value == 1)
assert(NumberSequence.new(0.5).Keypoints[2].Value == 0.5)
local k = NumberSequenceKeypoint.new(0.5, 2, 0.1)
assert(typeof(k) == "NumberSequenceKeypoint" and k.Envelope - 0.1 < 1e-6)
assert(NumberSequenceKeypoint.new(0, 1).Envelope == 0)
local s3 = NumberSequence.new({ NumberSequenceKeypoint.new(0, 0), k, NumberSequenceKeypoint.new(1, 0) })
assert(#s3.Keypoints == 3 and s3.Keypoints[2] == k)
assert(not pcall(NumberSequence.new, { NumberSequenceKeypoint.new(0, 0) }))
assert(not pcall(NumberSequence.new, { NumberSequenceKeypoint.new(0, 0), NumberSequenceKeypoint.new(0.5, 0) }))
assert(not pcall(NumberSequence.new, {
	NumberSequenceKeypoint.new(0, 0),
	NumberSequenceKeypoint.new(0.6, 0),
	NumberSequenceKeypoint.new(0.4, 0),
	NumberSequenceKeypoint.new(1, 0),
}))
assert(s == NumberSequence.new(0, 1))
assert(tostring(s) == "0 0 0 1 1 0 ")
