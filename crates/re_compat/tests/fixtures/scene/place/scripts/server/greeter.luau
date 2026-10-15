-- A synthetic fixture: the scene tests load and save the scene only.
local ReplicatedStorage = game:GetService("ReplicatedStorage")
local Strings = require(ReplicatedStorage.Shared.Strings)
print(Strings.hello)
