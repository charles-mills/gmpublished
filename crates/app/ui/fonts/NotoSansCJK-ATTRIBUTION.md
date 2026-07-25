# CJK Fallback Font Attribution

The bundled CJK fallback fonts are Modified Versions generated from
unmodified files from the official `notofonts/noto-cjk` repository:

- Original `NotoSansSC-Regular.otf`
  - Source: `https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans/SubsetOTF/SC/NotoSansSC-Regular.otf`
  - SHA-256: `faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9`
- Original `NotoSansKR-Regular.otf`
  - Source: `https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans/SubsetOTF/KR/NotoSansKR-Regular.otf`
  - SHA-256: `69975a0ac8472717870aefeab0a4d52739308d90856b9955313b2ad5e0148d68`

Bundled generated files:

- `GMPCJKSCUI-Regular.otf`
  - Generated from `NotoSansSC-Regular.otf`
  - SHA-256: `9821be02f2ff66ae9ddbc1265a27e6999a691fa7278ea77bd763f3e3abb3fca4`
  - OpenType family/full name: `GMP CJKSC UI`
  - PostScript name: `GMPCJKSCUI-Regular`
- `GMPCJKKRUI-Regular.otf`
  - Generated from `NotoSansKR-Regular.otf`
  - SHA-256: `7c75964751707633ff893ed513b22c2ded65e4d3cb080b12d044a619f65d9a0b`
  - OpenType family/full name: `GMP CJKKR UI`
  - PostScript name: `GMPCJKKRUI-Regular`

Generation:

```sh
packaging/scripts/subset_cjk_fonts.py \
  --sc-source /path/to/NotoSansSC-Regular.otf \
  --kr-source /path/to/NotoSansKR-Regular.otf
```

The script derives the required glyph set from `crates/app/i18n/zh-cn.ftl`
and `crates/app/i18n/kr.ftl`, plus the CJK sample strings used by the font
tests, then runs HarfBuzz `hb-subset --no-hinting`. It also renames the
primary OpenType `name` records and CFF-visible names so the Modified Versions
do not present themselves as Noto fonts.

Coverage tradeoff: these are UI/catalog subsets for the shipped Simplified
Chinese and Korean app translations. They do not provide arbitrary CJK Unicode
coverage for user-supplied Workshop titles or other external content.

License: SIL Open Font License 1.1. See `NotoSansCJK-OFL.txt` in this
directory.
