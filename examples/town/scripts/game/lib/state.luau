--!strict
-- State the game's scripts share. Modules load once per context, so every
-- script that requires this gets the same table.
--
-- mode: "title" (the autopilot drives while the title shows), "shift" (the
--   player works a shift) or "results" (the shift's score).
-- paused: the shift is paused (the simulation stands still).
-- driver: who drives the truck: "player", "autopilot" or "parked".
-- pilot: the autopilot's route, while it drives (lib/autopilot).
-- view: the camera: "chase", "above" or "orbit".
-- jolt: the hardest knock the truck took in the last step, in m/s of
--   sudden change in velocity (truck.luau), for the cargo.
-- speed, rpm, gear: the truck's after the last step.
-- teleport: where to put the truck at the next step (and which way it
--   faces), for a new shift or a reset.
-- town_seed: the seed of the town being driven.
local state = {
	mode = "title",
	paused = false,
	driver = "parked",
	pilot = nil :: any,
	view = "orbit",
	jolt = 0,
	speed = 0,
	rpm = 0,
	gear = 0,
	teleport = nil :: { at: vector, rotation: Quat }?,
	town_seed = 1,
}
return state
