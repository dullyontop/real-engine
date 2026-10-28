# Font substitutes

`substitutes.json` maps each font family a script can name (the stem of
`rbxasset://fonts/families/<Stem>.json`), and each legacy `Enum.Font` item, to
one of the three open-licence families the UI core bundles
(`crates/re_ui/assets/fonts`, ADR-0035). A family that is not listed uses the
default, Source Sans 3.

These are not the fonts the original platform ships. Text drawn with them has
different glyph shapes and widths, so `TextBounds` and wrapping can differ from
the original by a few pixels.
