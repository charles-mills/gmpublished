# CJK Fallback Font Attribution

The bundled CJK fallback fonts are Modified Versions generated from
unmodified files from the official `notofonts/noto-cjk` repository:

- Original `NotoSansSC-Regular.otf`
  - Source: `https://raw.githubusercontent.com/notofonts/noto-cjk/main/Sans/SubsetOTF/SC/NotoSansSC-Regular.otf`
  - SHA-256: `faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9`
- Original `NotoSansKR-Regular.otf`
  - Source: `https://raw.githubusercontent.com/notofonts/noto-cjk/main/Sans/SubsetOTF/KR/NotoSansKR-Regular.otf`
  - SHA-256: `69975a0ac8472717870aefeab0a4d52739308d90856b9955313b2ad5e0148d68`

Bundled generated files:

- `GMPCJKSCUI-Regular.otf`
  - Generated from `NotoSansSC-Regular.otf`
  - SHA-256: `186580ec027f6f2ffab322f0cc943284d863fcd020ade0d075b39a51aa489bb4`
  - OpenType family/full name: `GMP CJKSC UI`
  - PostScript name: `GMPCJKSCUI-Regular`
- `GMPCJKKRUI-Regular.otf`
  - Generated from `NotoSansKR-Regular.otf`
  - SHA-256: `60c8ae7cbe12190e3cdbdf6251c547a1a031b73b95f58bbaebfd297f6046e656`
  - OpenType family/full name: `GMP CJKKR UI`
  - PostScript name: `GMPCJKKRUI-Regular`

Generation:

```sh
packaging/scripts/subset_cjk_fonts.py \
  --sc-source /path/to/NotoSansSC-Regular.otf \
  --kr-source /path/to/NotoSansKR-Regular.otf
```

The script derives the required glyph set from `crates/app/i18n/zh-cn.json`
and `crates/app/i18n/kr.json`, plus the CJK sample strings used by the font
tests, then runs HarfBuzz `hb-subset --no-hinting`. It also renames the
primary OpenType `name` records and CFF-visible names so the Modified Versions
do not present themselves as Noto fonts.

Coverage tradeoff: these are UI/catalog subsets for the shipped Simplified
Chinese and Korean app translations. They do not provide arbitrary CJK Unicode
coverage for user-supplied Workshop titles or other external content.

License: SIL Open Font License 1.1. See `NotoSansCJK-OFL.txt` in this
directory.
