# Bundled fonts

Three open-licence font families the UI core draws text with (ADR-0035,
ADR-0058). Native UI names them directly; the compat GUI maps the families
scripts name to them (`crates/re_compat/assets/fonts/substitutes.json`). For a
weight with no file, the nearest weight is used.

| Family | Weights | Source package | Licence |
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
