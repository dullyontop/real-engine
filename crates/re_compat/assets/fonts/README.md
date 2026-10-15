# Bundled fonts

Open-licence substitutes for the font families compat UI names (ADR-0035).
`substitutes.json` maps each family, and each legacy `Enum.Font` item, to one of
three substitutes. A family that is not listed uses the default, Source Sans 3.
For a weight with no file, the nearest weight is used.

| Substitute | Weights | Source package | Licence |
|---|---|---|---|
| Montserrat | 400, 500, 700, 900 | `@fontsource/montserrat` 5.3.0 (npm) | SIL OFL 1.1, `OFL-Montserrat.txt` |
| Source Sans 3 | 400, 700 | `@fontsource/source-sans-3` 5.3.1 (npm) | SIL OFL 1.1, `OFL-SourceSans3.txt` |
| Roboto Mono | 400, 700 | `@fontsource/roboto-mono` 5.3.0 (npm) | SIL OFL 1.1, `OFL-RobotoMono.txt` |

Each `.ttf` file is the package's `files/<name>-latin-<weight>-normal.woff`
(the Latin subset), converted from WOFF 1.0 to TrueType by
`tools/woff2ttf.py`. The script only unpacks the zlib-compressed tables and
writes them back as a plain sfnt; glyph data is unchanged. To regenerate one:

```sh
python3 tools/woff2ttf.py montserrat-latin-700-normal.woff montserrat-latin-700.ttf
```

These are not the fonts the original platform ships. Text drawn with them has
different glyph shapes and widths, so `TextBounds` and wrapping can differ from
the original by a few pixels.
