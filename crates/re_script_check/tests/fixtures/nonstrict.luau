-- No mode directive: the project's .luaurc (nonstrict) applies.
local coins = world.query("Collectible")
for i, coin in coins do
	coin:set("Collectible", "value", i * 10)
end
time.on_step(function(dt)
	print(#coins, dt)
end)
